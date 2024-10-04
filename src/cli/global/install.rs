use std::str::FromStr;

use clap::Parser;
use indexmap::IndexMap;
use miette::{Context, IntoDiagnostic};
use rattler_conda_types::{MatchSpec, NamedChannelOrUrl, PackageName, Platform};

use crate::{
    cli::{global::revert_environment_after_error, has_specs::HasSpecs},
    global::{self, EnvironmentName, ExposedName, Mapping, Project},
    prefix::Prefix,
};
use pixi_config::{self, Config, ConfigCli};

/// Installs the defined package in a globally accessible location.
#[derive(Parser, Debug, Clone)]
#[clap(arg_required_else_help = true)]
pub struct Args {
    /// Specifies the packages that are to be installed.
    #[arg(num_args = 1..)]
    packages: Vec<String>,

    /// The channels to consider as a name or a url.
    /// Multiple channels can be specified by using this field multiple times.
    ///
    /// When specifying a channel, it is common that the selected channel also
    /// depends on the `conda-forge` channel.
    ///
    /// By default, if no channel is provided, `conda-forge` is used.
    #[clap(long = "channel", short = 'c', value_name = "CHANNEL")]
    channels: Vec<NamedChannelOrUrl>,

    #[clap(short, long)]
    platform: Option<Platform>,

    /// Ensures that all packages will be installed in the same environment
    #[clap(short, long)]
    environment: Option<EnvironmentName>,

    /// Add one or more mapping which describe which executables are exposed.
    /// The syntax is `exposed_name=executable_name`, so for example `python3.10=python`.
    /// Alternatively, you can input only an executable_name and `executable_name=executable_name` is assumed.
    #[arg(long)]
    expose: Vec<Mapping>,

    /// Answer yes to all questions.
    #[clap(short = 'y', long = "yes", long = "assume-yes")]
    assume_yes: bool,

    #[clap(flatten)]
    config: ConfigCli,
}

impl HasSpecs for Args {
    fn packages(&self) -> Vec<&str> {
        self.packages.iter().map(AsRef::as_ref).collect()
    }
}

pub async fn execute(args: Args) -> miette::Result<()> {
    let config = Config::with_cli_config(&args.config);
    let project_original = global::Project::discover_or_create(args.assume_yes)
        .await?
        .with_cli_config(config.clone());

    let env_names = match &args.environment {
        Some(env_name) => Vec::from([env_name.clone()]),
        None => args
            .specs()?
            .iter()
            .map(|(package_name, _)| package_name.as_normalized().parse().into_diagnostic())
            .collect::<miette::Result<Vec<_>>>()?,
    };

    let multiple_envs = env_names.len() > 1;

    if !args.expose.is_empty() && env_names.len() != 1 {
        miette::bail!("Cannot add exposed mappings for more than one environment");
    }

    let mut project = project_original.clone();
    let specs = args.specs()?;
    for env_name in &env_names {
        let specs = if multiple_envs {
            specs
                .clone()
                .into_iter()
                .filter(|(package_name, _)| env_name.as_str() == package_name.as_source())
                .collect()
        } else {
            specs.clone()
        };

        if let Err(err) = setup_environment(env_name, &args, &mut project, specs).await {
            if project_original.environment(env_name).is_some() {
                revert_environment_after_error(env_name, &project_original)
                    .await
                    .wrap_err("Could not install packages. Reverting also failed.")?;
            }
            return Err(err);
        }
    }

    project.manifest.save().await?;

    Ok(())
}

async fn setup_environment(
    env_name: &EnvironmentName,
    args: &Args,
    project: &mut Project,
    specs: IndexMap<PackageName, MatchSpec>,
) -> miette::Result<()> {
    // Modify the project to include the new environment
    if project.manifest.parsed.envs.contains_key(env_name) {
        project.manifest.remove_environment(env_name)?;
    }

    let channels = if args.channels.is_empty() {
        project.config().default_channels()
    } else {
        args.channels.clone()
    };
    project.manifest.add_environment(env_name, Some(channels))?;

    if let Some(platform) = args.platform {
        project.manifest.set_platform(env_name, platform)?;
    }

    // Add the dependencies to the environment
    for (_package_name, spec) in &specs {
        project.manifest.add_dependency(
            env_name,
            spec,
            project.clone().config().global_channel_config(),
        )?;
    }

    // Installing the environment to be able to find the bin paths later
    project.install_environment(env_name).await?;

    if args.expose.is_empty() {
        // Add the expose binaries for all the packages that were requested to the manifest
        for (package_name, _spec) in &specs {
            let prefix = project.environment_prefix(env_name.clone()).await?;
            let prefix_package = prefix.find_designated_package(package_name).await?;
            let package_executables = prefix.find_executables(&[prefix_package]);
            for (executable_name, _) in &package_executables {
                let mapping = Mapping::new(
                    ExposedName::from_str(executable_name)?,
                    executable_name.clone(),
                );
                project.manifest.add_exposed_mapping(env_name, &mapping)?;
            }
            // If no executables were found, automatically expose the package name itself from the other packages.
            // This is useful for packages like `ansible` and `jupyter` which don't ship executables their own executables.
            if !package_executables
                .iter()
                .any(|(name, _)| name.as_str() == package_name.as_normalized())
            {
                if let Some((mapping, source_package_name)) =
                    find_binary_by_name(&prefix, package_name).await?
                {
                    project.manifest.add_exposed_mapping(env_name, &mapping)?;
                    tracing::warn!(
                        "Automatically exposed `{}` from `{}`",
                        mapping.exposed_name(),
                        source_package_name.as_normalized()
                    );
                }
            }
        }
    } else {
        // Only add the exposed mappings that were requested
        for mapping in &args.expose {
            project.manifest.add_exposed_mapping(env_name, mapping)?;
        }
    }

    // Expose executables of the new environment
    project
        .expose_executables_from_environment(env_name)
        .await?;
    Ok(())
}

/// Finds the package name in the prefix and automatically exposes it if an executable is found.
/// This is useful for packages like `ansible` and `jupyter` which don't ship executables their own executables.
/// This function will return the mapping and the package name of the package in which the binary was found.
async fn find_binary_by_name(
    prefix: &Prefix,
    package_name: &PackageName,
) -> miette::Result<Option<(Mapping, PackageName)>> {
    let installed_packages = prefix.find_installed_packages(None).await?;
    for package in &installed_packages {
        let executables = prefix.find_executables(&[package.clone()]);

        // Check if any of the executables match the package name
        if let Some(executable) = executables
            .iter()
            .find(|(name, _)| name.as_str() == package_name.as_normalized())
        {
            return Ok(Some((
                Mapping::new(ExposedName::from_str(&executable.0)?, executable.0.clone()),
                package.repodata_record.package_record.name.clone(),
            )));
        }
    }
    Ok(None)
}
