use itertools::Itertools;
use serde::Deserialize;
use serde_with::{formats::PreferMany, serde_as, OneOrMany};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

/// Represents different types of scripts
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Task {
    Plain(String),
    Execute(Execute),
    Alias(Alias),
}

impl Task {
    /// Returns the names of the task that this task depends on
    pub fn depends_on(&self) -> &[String] {
        match self {
            Task::Plain(_) => &[],
            Task::Execute(cmd) => &cmd.depends_on,
            Task::Alias(cmd) => &cmd.depends_on,
        }
    }

    /// If this task is a plain task, returns the task string
    pub fn as_plain(&self) -> Option<&String> {
        match self {
            Task::Plain(str) => Some(str),
            _ => None,
        }
    }

    // If this command is an execute command, returns the [`Execute`] task.
    pub fn as_execute(&self) -> Option<&Execute> {
        match self {
            Task::Execute(execute) => Some(execute),
            _ => None,
        }
    }

    /// If this command is an alias, returns the [`Alias`] task.
    pub fn as_alias(&self) -> Option<&Alias> {
        match self {
            Task::Alias(alias) => Some(alias),
            _ => None,
        }
    }

    /// Returns true if this task is directly executable
    pub fn is_executable(&self) -> bool {
        match self {
            Task::Plain(_) | Task::Execute(_) => true,
            Task::Alias(_) => false,
        }
    }

    /// Returns the command to execute.
    pub fn as_command(&self) -> Option<CmdArgs> {
        match self {
            Task::Plain(cmd) => Some(CmdArgs::Single(cmd.clone())),
            Task::Execute(exe) => Some(exe.cmd.clone()),
            Task::Alias(_) => None,
        }
    }

    /// Returns the command to execute as a single string.
    pub fn as_single_command(&self) -> Option<Cow<str>> {
        match self {
            Task::Plain(cmd) => Some(Cow::Borrowed(cmd)),
            Task::Execute(exe) => Some(exe.cmd.as_single()),
            Task::Alias(_) => None,
        }
    }
}

/// A command script executes a single command from the environment
#[serde_as]
#[derive(Debug, Clone, Deserialize)]
pub struct Execute {
    /// A list of arguments, the first argument denotes the command to run. When deserializing both
    /// an array of strings and a single string are supported.
    pub cmd: CmdArgs,

    /// A list of commands that should be run before this one
    #[serde(default)]
    #[serde_as(deserialize_as = "OneOrMany<_, PreferMany>")]
    pub depends_on: Vec<String>,
}

impl From<Execute> for Task {
    fn from(value: Execute) -> Self {
        Task::Execute(value)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CmdArgs {
    Single(String),
    Multiple(Vec<String>),
}

impl From<Vec<String>> for CmdArgs {
    fn from(value: Vec<String>) -> Self {
        CmdArgs::Multiple(value)
    }
}

impl From<String> for CmdArgs {
    fn from(value: String) -> Self {
        CmdArgs::Single(value)
    }
}

impl CmdArgs {
    /// Returns a single string representation of the command arguments.
    pub fn as_single(&self) -> Cow<str> {
        match self {
            CmdArgs::Single(cmd) => Cow::Borrowed(cmd),
            CmdArgs::Multiple(args) => Cow::Owned(args.iter().map(|arg| quote(arg)).join(" ")),
        }
    }

    /// Returns a single string representation of the command arguments.
    pub fn into_single(self) -> String {
        match self {
            CmdArgs::Single(cmd) => cmd,
            CmdArgs::Multiple(args) => args.iter().map(|arg| quote(arg)).join(" "),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde_as]
pub struct Alias {
    /// A list of commands that should be run before this one
    #[serde_as(deserialize_as = "OneOrMany<_, PreferMany>")]
    pub depends_on: Vec<String>,
}

impl Display for Task {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Task::Plain(cmd) => {
                write!(f, "{}", cmd)?;
            }
            Task::Execute(cmd) => {
                match &cmd.cmd {
                    CmdArgs::Single(cmd) => write!(f, "{}", cmd)?,
                    CmdArgs::Multiple(mult) => write!(f, "{}", mult.join(" "))?,
                };
                if !cmd.depends_on.is_empty() {
                    write!(f, ", ")?;
                }
            }
            _ => {}
        };

        let depends_on = self.depends_on();
        if !depends_on.is_empty() {
            if depends_on.len() == 1 {
                write!(f, "depends_on = '{}'", depends_on.iter().join(","))
            } else {
                write!(f, "depends_on = [{}]", depends_on.iter().join(","))
            }
        } else {
            Ok(())
        }
    }
}

/// Quotes a string argument if it requires quotes to be able to be properly represented in our
/// shell implementation.
pub fn quote(in_str: &str) -> Cow<str> {
    if in_str.is_empty() {
        "\"\"".into()
    } else if in_str
        .bytes()
        .any(|c| matches!(c as char, '\t' | '\r' | '\n' | ' '))
    {
        let mut out: Vec<u8> = Vec::new();
        out.push(b'"');
        for c in in_str.bytes() {
            match c as char {
                '"' | '\\' => out.push(b'\\'),
                _ => (),
            }
            out.push(c);
        }
        out.push(b'"');
        unsafe { String::from_utf8_unchecked(out) }.into()
    } else {
        in_str.into()
    }
}

/// Quotes multiple string arguments and joins them together to form a single string.
pub fn quote_arguments<'a>(args: impl IntoIterator<Item = &'a str>) -> String {
    args.into_iter().map(quote).join(" ")
}

#[cfg(test)]
mod test {
    use super::quote;

    #[test]
    fn test_quote() {
        assert_eq!(quote("foobar"), "foobar");
        assert_eq!(quote("foo bar"), "\"foo bar\"");
        assert_eq!(quote("\""), "\"");
        assert_eq!(quote("foo \" bar"), "\"foo \\\" bar\"");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("$PATH"), "$PATH");
        assert_eq!(
            quote("PATH=\"$PATH;build/Debug\""),
            "PATH=\"$PATH;build/Debug\""
        );
    }
}