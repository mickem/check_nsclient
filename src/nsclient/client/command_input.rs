use crate::nsclient::messages::QueryHelp;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::HashMap;
use tui_prompts::FocusState::Focused;
use tui_prompts::{State, TextState};

/// Built-in commands understood by `parse_command` (anything else is treated as a query name).
const VALID_COMMANDS: &[&str] = &[
    "ping", "version", "query", "modules", "refresh", "history", "exit", "help", "queries", "list",
    "load", "unload", "plugins",
];

const MAX_HISTORY_LENGTH: usize = 30;

#[derive(Debug)]
pub struct QueryCommand {
    pub command: String,
    pub args: Vec<(String, String)>,
}

impl QueryCommand {
    fn from_tokens(tokens: &[String]) -> anyhow::Result<Self> {
        let Some((command, args)) = tokens.split_first() else {
            anyhow::bail!("Invalid syntax: query <name> [key=value...]");
        };
        Ok(Self {
            command: command.clone(),
            args: parse_arguments(args),
        })
    }
}

#[derive(Debug)]
pub enum ModuleCommand {
    Load(String),
    Unload(String),
    List,
}

fn parse_arguments(args: &[String]) -> Vec<(String, String)> {
    let mut map = Vec::new();
    for arg in args {
        if let Some((key, value)) = arg.split_once('=') {
            map.push((key.to_string(), value.to_string()));
        } else {
            map.push((arg.to_string(), String::new()));
        }
    }
    map
}

#[derive(Debug)]
pub enum HistoryCommand {
    List,
    Clear,
    Delete(usize),
}

#[derive(Debug)]
pub enum CommandType {
    Ping,
    Version,
    Refresh,
    Exit,
    Help,
    Query(QueryCommand),
    Queries,
    History(HistoryCommand),
    Module(ModuleCommand),
}
#[derive(Debug)]
pub struct Command {
    pub command: CommandType,
}

/// What pressing Tab did.
#[derive(Debug, PartialEq, Eq)]
pub enum Completion {
    /// Nothing to complete against, or nothing matched: the input is unchanged.
    Nothing,
    /// The input was extended -- to the single match, or to the prefix every
    /// match shares.
    Extended,
    /// Several things match and they share no longer prefix, so the caller
    /// shows them and lets the user pick.
    Candidates(Vec<Suggestion>),
}

/// One thing Tab could put in, and what the agent says it is for.
#[derive(Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The text that goes in, `=` and all.
    pub text: String,
    /// The agent's summary line for it, empty when it sent none.
    pub description: String,
}

impl Suggestion {
    fn described(text: impl Into<String>, description: String) -> Self {
        Self {
            text: text.into(),
            description,
        }
    }

    /// For something the agent says nothing about, such as a command name.
    fn bare(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            description: String::new(),
        }
    }
}

#[derive(Debug)]
pub struct CommandInput<'a> {
    command_state: TextState<'a>,
    available_commands: Vec<String>,
    history: Vec<String>,
    history_index: Option<usize>,
    /// The text that was being typed before the user started browsing history with Up/Down;
    /// restored when browsing past the most recent entry.
    draft: Option<String>,
    /// What each query accepts, as the agent described it.
    ///
    /// Keyed in lower case: the agent matches command names without regard to
    /// case and so does the prompt, so a cache that did not would treat
    /// `Check_CPU` and `check_cpu` as two different queries and ask twice.
    ///
    /// An entry is put in as `None` the moment the query is asked about and
    /// overwritten when the answer lands, so a present entry means "asked, do
    /// not ask again" and a `None` one means "nothing to offer" -- which is
    /// equally true while the answer is still in flight and if the agent had
    /// nothing to say. A map rather than one slot, so moving between two
    /// commands on consecutive lines does not ask about each one every time.
    help: HashMap<String, Option<QueryHelp>>,
    /// A command whose help the UI should go and fetch. Taken by the caller,
    /// which is the side that can talk to the agent.
    wanted_help: Option<String>,
}

impl<'a> CommandInput<'a> {
    pub(crate) fn new(history: Vec<String>) -> Self {
        CommandInput {
            command_state: TextState::new().with_focus(Focused),
            available_commands: Vec::new(),
            history,
            history_index: None,
            draft: None,
            help: HashMap::new(),
            wanted_help: None,
        }
    }

    /// The command whose help the UI should fetch, if any. Clears on read, so
    /// each command name is asked about once.
    pub fn take_help_request(&mut self) -> Option<String> {
        self.wanted_help.take()
    }

    /// Record what the agent said a query accepts (or had nothing to say about).
    pub fn on_query_help(&mut self, command: &str, help: Option<QueryHelp>) {
        self.help.insert(help_key(command), help);
    }

    /// Forget that a query was ever asked about, so the next keystroke past its
    /// name asks again.
    ///
    /// For a request that failed rather than one the agent answered: a timeout
    /// or a token that needed refreshing says nothing about the query, and
    /// remembering it would turn a blip into completion that is dead for the
    /// rest of the session with nothing on screen to explain it.
    pub fn forget_query_help(&mut self, command: &str) {
        self.help.remove(&help_key(command));
    }

    pub(crate) fn get_history(&self) -> Vec<String> {
        self.history.clone()
    }

    pub fn update_commands(&mut self, commands: Vec<String>) {
        self.available_commands = commands;
    }

    pub(crate) fn get_state(&mut self) -> &mut TextState<'a> {
        &mut self.command_state
    }

    pub fn has_value(&self) -> bool {
        let value = self.command_state.value();
        !value.trim().is_empty()
    }

    fn add_history(&mut self, value: &str) {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        self.history.retain(|x| x != value);
        self.history.push(value.to_owned());
        if self.history.len() > MAX_HISTORY_LENGTH {
            self.history.remove(0);
        }
    }

    /// Parse the current input into a command.
    ///
    /// Only commands that parse successfully are recorded in the history and clear the input;
    /// an invalid command is left in place so the user can correct it.
    pub fn get_command(&mut self) -> anyhow::Result<Command> {
        let value = self.command_state.value().to_owned();
        self.history_index = None;
        self.draft = None;

        let tokens = tokenize_command(&value)?;
        if tokens.is_empty() || !is_valid_command(&tokens[0], &self.available_commands) {
            anyhow::bail!("Invalid command")
        }
        let command = parse_command(&tokens)?;

        self.add_history(&value);
        self.command_state.truncate();
        self.set_status_pending();
        Ok(command)
    }

    fn set_status_ok(&mut self) {
        *self.command_state.status_mut() = tui_prompts::Status::Done;
    }
    fn set_status_error(&mut self) {
        *self.command_state.status_mut() = tui_prompts::Status::Aborted;
    }
    fn set_status_pending(&mut self) {
        *self.command_state.status_mut() = tui_prompts::Status::Pending;
    }

    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let new_index = match self.history_index {
            None => {
                self.draft = Some(self.command_state.value().to_owned());
                self.history.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.history_index = Some(new_index);
        self.update_input_from_history();
    }

    fn history_down(&mut self) {
        let Some(i) = self.history_index else {
            return;
        };
        if i + 1 < self.history.len() {
            self.history_index = Some(i + 1);
            self.update_input_from_history();
        } else {
            self.history_index = None;
            let draft = self.draft.take().unwrap_or_default();
            self.set_input(&draft);
        }
    }

    fn set_input(&mut self, text: &str) {
        self.command_state.truncate();
        self.command_state.value_mut().push_str(text);
        self.command_state.move_end();
    }

    pub(crate) fn handle_history(&mut self, command: HistoryCommand) -> Vec<String> {
        match command {
            HistoryCommand::List => self
                .history
                .iter()
                .enumerate()
                .map(|(i, cmd)| format!("{i}: {cmd}"))
                .collect(),
            HistoryCommand::Clear => {
                self.history.clear();
                vec!["History cleared".into()]
            }
            HistoryCommand::Delete(index) => {
                if index >= self.history.len() {
                    return vec![format!(
                        "Invalid history index {index} (history has {} entries)",
                        self.history.len()
                    )];
                }
                self.history.remove(index);
                vec!["History deleted".into()]
            }
        }
    }

    fn update_input_from_history(&mut self) {
        if let Some(idx) = self.history_index
            && let Some(cmd) = self.history.get(idx).cloned()
        {
            self.set_input(&cmd);
        }
    }

    pub(crate) fn handle_key_event(&mut self, event: KeyEvent) {
        if event.kind == KeyEventKind::Release {
            return;
        }

        match (event.code, event.modifiers) {
            (KeyCode::Up, _) => {
                self.history_up();
                return;
            }
            (KeyCode::Down, _) => {
                self.history_down();
                return;
            }
            (KeyCode::Left, _) | (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.command_state.move_left();
                return;
            }
            (KeyCode::Right, _) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.command_state.move_right();
                return;
            }
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.command_state.move_start();
                return;
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.command_state.move_end();
                return;
            }
            (KeyCode::Backspace, _) | (KeyCode::Char('h'), KeyModifiers::CONTROL) => {
                self.command_state.backspace();
            }
            (KeyCode::Delete, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.command_state.delete();
            }
            (KeyCode::Char(c), _) => {
                self.command_state.push(c);
            }
            _ => {
                return;
            }
        }
        let value = self.command_state.value();
        if value.trim().is_empty() {
            self.set_status_pending();
            return;
        }

        match tokenize_command(value) {
            Ok(tokens) if !tokens.is_empty() => {
                if is_valid_command(&tokens[0], &self.available_commands) {
                    self.set_status_ok();
                } else {
                    self.set_status_error();
                }
            }
            _ => self.set_status_error(),
        }
        self.note_help_needed();
    }

    /// Ask for a query's vocabulary once the user starts on its arguments.
    ///
    /// Waiting for the space after the command name means one request per
    /// command rather than one per keystroke while the name is still being
    /// typed, and it is exactly the point where completing an argument starts
    /// to be worth anything.
    fn note_help_needed(&mut self) {
        let value = self.command_state.value();
        let Some(command) = command_being_argued(value) else {
            return;
        };
        // Already asked -- answered or not.
        if self.help.contains_key(&help_key(command)) {
            return;
        }
        // The prompt accepts a command name in any case and so does the agent,
        // so `Check_CPU` has to reach this too.
        if !self
            .available_commands
            .iter()
            .any(|known| known.eq_ignore_ascii_case(command))
        {
            return;
        }
        let command = command.to_owned();
        // Entered now, so the keystrokes between asking and being answered do
        // not each ask again.
        self.help.insert(help_key(&command), None);
        self.wanted_help = Some(command);
    }

    /// Complete the word at the end of the input.
    ///
    /// Completion works on the end of the line rather than at the cursor: this
    /// is a prompt, the cursor is at the end whenever someone is typing, and
    /// the alternative is splicing text mid-value for a case that does not
    /// arise.
    pub fn complete(&mut self) -> Completion {
        let value = self.command_state.value().to_owned();
        let (partial, candidates) = self.candidates(&value);
        self.apply(&value, partial, candidates)
    }

    /// What Tab could put in at the end of `value`, and the text it replaces.
    ///
    /// That text is the word being completed, except inside a filter
    /// expression, where it is the keyword being typed within the value rather
    /// than the whole `filter=...` word.
    fn candidates<'v>(&self, value: &'v str) -> (&'v str, Vec<Suggestion>) {
        // Still on the first word: the commands themselves.
        let Some(command) = command_being_argued(value) else {
            let partial = last_word(value);
            let candidates = self
                .available_commands
                .iter()
                .map(String::as_str)
                .chain(VALID_COMMANDS.iter().copied())
                .filter(|name| name.starts_with(partial))
                .map(Suggestion::bare)
                .collect();
            return (partial, candidates);
        };
        // Past it: what the agent said this query accepts.
        let Some(Some(help)) = self.help.get(&help_key(command)) else {
            return (last_word(value), Vec::new());
        };
        // A filter expression is written in the check's keywords, so inside one
        // they are the vocabulary and its options are not.
        if let Some(keyword) = filter_keyword_being_typed(value) {
            let candidates = help
                .fields
                .iter()
                // A keyword may be named twice in one expression, so unlike an
                // option, one already on the line is still offered.
                .filter(|field| field.name.starts_with(keyword))
                .map(|field| {
                    Suggestion::described(
                        &field.name,
                        summary(&field.short_description, &field.long_description),
                    )
                })
                .collect();
            return (keyword, candidates);
        }
        let partial = last_word(value);
        // Everything before the word being completed. The word itself is what
        // Tab was asked to finish, so it is never treated as already given.
        let settled = &value[..value.len() - partial.len()];
        let candidates = help
            .parameters
            .iter()
            .filter(|p| p.name.starts_with(partial))
            // An option already settled on the line is not offered again,
            // unless the check says it may be repeated.
            .filter(|p| p.repeatable || !argument_already_given(settled, &p.name))
            .map(|p| {
                Suggestion::described(
                    suggest_argument(p),
                    summary(&p.short_description, &p.long_description),
                )
            })
            .collect();
        (partial, candidates)
    }

    /// Put a completion in, or report back what the user has to choose between.
    fn apply(&mut self, value: &str, partial: &str, candidates: Vec<Suggestion>) -> Completion {
        let Some(shared) = shared_prefix(&candidates) else {
            return Completion::Nothing;
        };
        // A single match completes outright; several extend only as far as they
        // agree, and if they agree on nothing more the user has to choose.
        if candidates.len() > 1 && shared.len() == partial.len() {
            return Completion::Candidates(candidates);
        }
        let head = &value[..value.len() - partial.len()];
        self.set_input(&format!("{head}{shared}"));
        Completion::Extended
    }

    pub fn handle_help(&mut self) -> Vec<String> {
        vec![
            "Available commands:".into(),
            "  help:    Show help".into(),
            "  exit:    Exit program (Esc)".into(),
            "  history: Show and manipulate history".into(),
            "  ping:    Check if backend is reachable".into(),
            "  version: Show backend version".into(),
            "  query:   Execute a query (check command)".into(),
            "  modules: Show and manipulate modules".into(),
            "  queries: List all available queries (check commands)".into(),
            "  ...      Any query (check command) can be executed as-is".into(),
            "".into(),
            "Tab completes a command name, and -- once you are past it -- the".into(),
            "options that check accepts, as the agent describes them. Inside".into(),
            "filter=, warning=, critical= and ok= it completes the filter".into(),
            "keywords the check offers instead.".into(),
        ]
    }
}

/// How a command name is keyed in the help cache.
///
/// The agent matches command names without regard to case, so the cache must
/// too, or `Check_CPU` and `check_cpu` would each be asked about separately.
fn help_key(command: &str) -> String {
    command.to_lowercase()
}

/// The first word of `value`, and everything from the end of it onwards.
///
/// The tail is taken from where the word ends rather than from a length
/// counted off the start of the string, which is what keeps this right for a
/// value that opens with spaces, and safe for one that opens with a character
/// wider than a byte. Slicing `value` by `first.len()` does neither: it reads
/// the wrong place after leading whitespace, and lands inside a character when
/// the first one is not ASCII.
fn split_first_word(value: &str) -> Option<(&str, &str)> {
    let start = value.find(|c: char| !c.is_whitespace())?;
    let rest = &value[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some((&rest[..end], &rest[end..]))
}

/// The command whose arguments are being typed, if the input has moved past
/// the command name.
///
/// `check_cpu` is still the name being typed; `check_cpu ` and `check_cpu w`
/// are arguments to `check_cpu`. A built-in such as `query check_cpu ` names
/// the query in the second word instead.
fn command_being_argued(value: &str) -> Option<&str> {
    let (first, rest) = split_first_word(value)?;
    if first.eq_ignore_ascii_case("query") {
        let (second, rest) = split_first_word(rest)?;
        return rest.starts_with(char::is_whitespace).then_some(second);
    }
    rest.starts_with(char::is_whitespace).then_some(first)
}

/// The partial word Tab should complete: what follows the last space.
fn last_word(value: &str) -> &str {
    let Some(at) = value.rfind(char::is_whitespace) else {
        return value;
    };
    // `rfind` gives where the whitespace character starts, and a whitespace
    // character is not always one byte -- a non-breaking space pasted in from a
    // web page is two. Step over the character, not over a byte.
    let width = value[at..].chars().next().map_or(1, char::len_utf8);
    &value[at + width..]
}

/// Is this option already settled on the line?
///
/// `settled` is the input up to the word being completed, never including it:
/// the half-typed word *is* what Tab was asked to finish, and counting it as
/// already given is how `check_cpu critical` + Tab came back with nothing
/// instead of `check_cpu critical=`.
fn argument_already_given(settled: &str, name: &str) -> bool {
    settled
        .split_whitespace()
        .skip(1)
        .any(|word| word == name || word.split_once('=').is_some_and(|(key, _)| key == name))
}

/// The options whose value is a filter expression rather than a plain value.
///
/// Nothing in the agent's answer marks them out -- `help` types every one of
/// them as a `string` -- so the set is the one NSClient++'s filter framework
/// adds to every filter based check: the selector, the three state filters, and
/// the two abbreviations the agent also accepts for them. `empty-state` is not
/// one of them; it takes a state, not an expression.
const FILTER_EXPRESSION_OPTIONS: &[&str] = &["filter", "warning", "warn", "critical", "crit", "ok"];

/// The filter keyword being typed at the end of `value`, if that is where the
/// line ends.
///
/// `filter=st` is asking about `state`, not about another option, so this is
/// what tells the two vocabularies apart. A filter expression holds spaces and
/// is therefore usually quoted, which is why this walks the line rather than
/// looking at its last word: inside an unclosed quote the keyword after an `and`
/// is still part of the `filter=` value.
///
/// `None` when the line does not end in such a value at all, and when it ends
/// inside a string literal (`filter="state = 'run`), where what comes next is
/// the rest of the literal rather than a keyword. The keyword itself is empty
/// where the expression ends on an operator or a space, which offers the lot.
fn filter_keyword_being_typed(value: &str) -> Option<&str> {
    // The option whose value the walk is inside, and where that value starts.
    let mut option: Option<(&str, usize)> = None;
    let mut word_start = 0;
    let mut quoted = false;
    for (at, c) in value.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c.is_whitespace() && !quoted {
            // The word ended, so whatever it was assigning ended with it.
            option = None;
            word_start = at + c.len_utf8();
        } else if c == '=' && option.is_none() {
            // The first `=` of a word divides it; later ones belong to the
            // expression (`filter=state=='x'`).
            option = Some((&value[word_start..at], at + 1));
        }
    }
    let (name, start) = option?;
    if !FILTER_EXPRESSION_OPTIONS
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
    {
        return None;
    }
    let expression = &value[start..];
    // An odd number of `'` leaves the line inside a string literal.
    if expression.matches('\'').count() % 2 == 1 {
        return None;
    }
    Some(keyword_fragment(expression))
}

/// The trailing run of keyword characters in `expression`.
///
/// A keyword is a name (`used_pct`) or a function the registry marks with `()`
/// (`convert_bytes()`), so a run of letters, digits and underscores is as much
/// of one as can already have been typed; anything else -- an operator, a space,
/// the `(` of a function -- ends the expression on a point where a keyword
/// begins, and the fragment is empty.
fn keyword_fragment(expression: &str) -> &str {
    let start = expression
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map_or(expression.len(), |(at, _)| at);
    &expression[start..]
}

/// The one line to show beside a suggestion.
///
/// The summary line when the agent sent one, and the first line of the long
/// description otherwise -- which is where a filter keyword's text tends to be,
/// the agent leaving its summary empty for those.
fn summary(short: &str, long: &str) -> String {
    if !short.is_empty() {
        return short.to_owned();
    }
    long.lines().next().unwrap_or_default().to_owned()
}

/// The choice to show when Tab cannot narrow it down any further.
///
/// Names on one line while there is nothing to say about any of them -- which is
/// what an agent that sends no descriptions leaves, and what this always did --
/// and a line each, name beside summary, as soon as there is.
pub fn candidate_lines(candidates: &[Suggestion]) -> Vec<String> {
    if candidates.is_empty() {
        return Vec::new();
    }
    if candidates.iter().all(|c| c.description.is_empty()) {
        return vec![
            candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("  "),
        ];
    }
    let width = candidates
        .iter()
        .map(|c| c.text.chars().count())
        .max()
        .unwrap_or(0);
    candidates
        .iter()
        .map(|c| {
            format!("  {:width$}  {}", c.text, c.description)
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// How an option should be offered.
///
/// Checks declare their flags so that REST can pass `show-all=true`, and a bare
/// `show-all` is refused with *does not take any arguments* -- so an option is
/// suggested with its `=` already there. The exception is a `bool` with no
/// default, which is a plain switch (`help`, `show-default`) and takes no value
/// at all; that one is offered with a trailing space instead.
fn suggest_argument(parameter: &crate::nsclient::messages::QueryParameter) -> String {
    if parameter.content_type == "bool" && parameter.default_value.is_empty() {
        format!("{} ", parameter.name)
    } else {
        format!("{}=", parameter.name)
    }
}

/// The longest prefix every candidate shares, or `None` if there are none.
fn shared_prefix(candidates: &[Suggestion]) -> Option<String> {
    let first = &candidates.first()?.text;
    let mut length = first.len();
    for other in &candidates[1..] {
        length = length.min(
            first
                .char_indices()
                .zip(other.text.char_indices())
                .take_while(|((_, a), (_, b))| a == b)
                .last()
                .map_or(0, |((i, c), _)| i + c.len_utf8()),
        );
    }
    Some(first[..length].to_owned())
}

fn parse_command(tokens: &[String]) -> anyhow::Result<Command> {
    if tokens.is_empty() {
        anyhow::bail!("No command provided");
    }
    match tokens[0].to_lowercase().as_str() {
        "ping" => Ok(Command {
            command: CommandType::Ping,
        }),
        "version" => Ok(Command {
            command: CommandType::Version,
        }),
        "refresh" => Ok(Command {
            command: CommandType::Refresh,
        }),
        "history" => Ok(Command {
            command: parse_history_command(&tokens[1..])?,
        }),
        "modules" => Ok(Command {
            command: parse_modules_command(&tokens[1..])?,
        }),
        "load" => Ok(Command {
            command: parse_module_arg(&tokens[1..], "load", ModuleCommand::Load)?,
        }),
        "unload" => Ok(Command {
            command: parse_module_arg(&tokens[1..], "unload", ModuleCommand::Unload)?,
        }),
        "plugins" => Ok(Command {
            command: CommandType::Module(ModuleCommand::List),
        }),
        "exit" => Ok(Command {
            command: CommandType::Exit,
        }),
        "help" => Ok(Command {
            command: CommandType::Help,
        }),
        "query" => Ok(Command {
            command: CommandType::Query(QueryCommand::from_tokens(&tokens[1..])?),
        }),
        "list" | "queries" => Ok(Command {
            command: CommandType::Queries,
        }),
        _ => Ok(Command {
            command: CommandType::Query(QueryCommand::from_tokens(tokens)?),
        }),
    }
}

fn parse_module_arg(
    args: &[String],
    name: &str,
    build: fn(String) -> ModuleCommand,
) -> anyhow::Result<CommandType> {
    match args {
        [module] => Ok(CommandType::Module(build(module.clone()))),
        _ => anyhow::bail!("Invalid syntax: {name} <module>"),
    }
}

fn parse_history_command(args: &[String]) -> anyhow::Result<CommandType> {
    if args.is_empty() {
        return Ok(CommandType::History(HistoryCommand::List));
    }
    match args[0].to_lowercase().as_str() {
        "list" => Ok(CommandType::History(HistoryCommand::List)),
        "clear" => Ok(CommandType::History(HistoryCommand::Clear)),
        "delete" => {
            if args.len() != 2 {
                anyhow::bail!("Invalid syntax: history delete <index>");
            }
            let index = match args[1].parse::<usize>() {
                Ok(value) => value,
                Err(e) => anyhow::bail!("Error: {e}"),
            };
            Ok(CommandType::History(HistoryCommand::Delete(index)))
        }
        &_ => anyhow::bail!("Invalid history command"),
    }
}

fn parse_modules_command(args: &[String]) -> anyhow::Result<CommandType> {
    if args.is_empty() {
        return Ok(CommandType::Module(ModuleCommand::List));
    }
    match args[0].to_lowercase().as_str() {
        "list" => Ok(CommandType::Module(ModuleCommand::List)),
        "load" => parse_module_arg(&args[1..], "modules load", ModuleCommand::Load),
        "unload" => parse_module_arg(&args[1..], "modules unload", ModuleCommand::Unload),
        &_ => anyhow::bail!("Invalid modules command"),
    }
}

fn is_valid_command(candidate: &str, queries: &[String]) -> bool {
    VALID_COMMANDS
        .iter()
        .any(|cmd| candidate.eq_ignore_ascii_case(cmd))
        | queries
            .iter()
            .any(|cmd| candidate.eq_ignore_ascii_case(cmd))
}

fn tokenize_command(input: &str) -> anyhow::Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape_next = false;

    for ch in input.chars() {
        if escape_next {
            current.push(ch);
            escape_next = false;
            continue;
        }
        match ch {
            '\\' => escape_next = true,
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }

    if escape_next {
        anyhow::bail!("Dangling escape character");
    }
    if in_quotes {
        anyhow::bail!("Unterminated quote");
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nsclient::messages::{QueryField, QueryParameter};

    #[test]
    fn command_input_tokenizer_handles_quotes() {
        let tokens = tokenize_command(r#"ping "example.com""#).unwrap();
        assert_eq!(tokens, vec!["ping".to_string(), "example.com".to_string()]);
    }

    #[test]
    fn command_input_tokenizer_handles_quotes_and_escapes() {
        let tokens = tokenize_command(r#"queries "\"with spaces\"" arg"#).unwrap();
        assert_eq!(
            tokens,
            vec![
                "queries".to_string(),
                "\"with spaces\"".to_string(),
                "arg".to_string()
            ]
        );
    }

    #[test]
    fn command_input_tokenizer_rejects_unterminated_quote() {
        assert_eq!(
            tokenize_command("\"incomplete").unwrap_err().to_string(),
            "Unterminated quote"
        );
    }

    #[test]
    fn command_input_accepts_valid_command_case_insensitive() {
        assert!(is_valid_command("PiNg", &[]));
        assert!(!is_valid_command("unknown", &[]));
        assert!(is_valid_command("ExtraCommand", &["ExtraCommand".into()]));
    }

    #[test]
    fn every_builtin_command_is_understood_by_the_parser() {
        for name in VALID_COMMANDS {
            let tokens = vec![name.to_string(), "arg".to_string()];
            let result = parse_command(&tokens);
            // Some commands reject the extra argument, but none may fall through to being
            // executed as a query of the same name.
            if let Ok(Command {
                command: CommandType::Query(query),
            }) = result
                && query.command == *name
            {
                panic!("built-in command {name} was parsed as a query");
            }
        }
    }

    fn tokens(input: &str) -> Vec<String> {
        tokenize_command(input).unwrap()
    }

    #[test]
    fn parse_command_handles_builtins() {
        assert!(matches!(
            parse_command(&tokens("PING")).unwrap().command,
            CommandType::Ping
        ));
        assert!(matches!(
            parse_command(&tokens("version")).unwrap().command,
            CommandType::Version
        ));
        assert!(matches!(
            parse_command(&tokens("refresh")).unwrap().command,
            CommandType::Refresh
        ));
        assert!(matches!(
            parse_command(&tokens("exit")).unwrap().command,
            CommandType::Exit
        ));
        assert!(matches!(
            parse_command(&tokens("help")).unwrap().command,
            CommandType::Help
        ));
        assert!(matches!(
            parse_command(&tokens("list")).unwrap().command,
            CommandType::Queries
        ));
        assert!(matches!(
            parse_command(&tokens("queries")).unwrap().command,
            CommandType::Queries
        ));
        assert!(matches!(
            parse_command(&tokens("plugins")).unwrap().command,
            CommandType::Module(ModuleCommand::List)
        ));
    }

    #[test]
    fn parse_command_rejects_empty_input() {
        assert!(parse_command(&[]).is_err());
    }

    #[test]
    fn parse_command_query_keyword_requires_a_name() {
        let err = parse_command(&tokens("query")).unwrap_err().to_string();
        assert!(err.contains("query <name>"), "{err}");
    }

    #[test]
    fn parse_command_query_keyword_and_bare_query_parse_arguments() {
        for input in [
            "query check_cpu warning=80 show-all",
            "check_cpu warning=80 show-all",
        ] {
            match parse_command(&tokens(input)).unwrap().command {
                CommandType::Query(query) => {
                    assert_eq!(query.command, "check_cpu");
                    assert_eq!(
                        query.args,
                        vec![
                            ("warning".to_string(), "80".to_string()),
                            ("show-all".to_string(), String::new()),
                        ]
                    );
                }
                _ => panic!("{input} did not parse as a query"),
            }
        }
    }

    #[test]
    fn parse_command_load_and_unload_require_exactly_one_module() {
        assert!(parse_command(&tokens("load")).is_err());
        assert!(parse_command(&tokens("unload")).is_err());
        assert!(parse_command(&tokens("load a b")).is_err());
        assert!(matches!(
            parse_command(&tokens("load CheckSystem")).unwrap().command,
            CommandType::Module(ModuleCommand::Load(ref m)) if m == "CheckSystem"
        ));
        assert!(matches!(
            parse_command(&tokens("unload CheckSystem")).unwrap().command,
            CommandType::Module(ModuleCommand::Unload(ref m)) if m == "CheckSystem"
        ));
    }

    #[test]
    fn parse_command_modules_subcommands() {
        assert!(matches!(
            parse_command(&tokens("modules")).unwrap().command,
            CommandType::Module(ModuleCommand::List)
        ));
        assert!(matches!(
            parse_command(&tokens("modules list")).unwrap().command,
            CommandType::Module(ModuleCommand::List)
        ));
        assert!(matches!(
            parse_command(&tokens("modules load X")).unwrap().command,
            CommandType::Module(ModuleCommand::Load(ref m)) if m == "X"
        ));
        assert!(parse_command(&tokens("modules load")).is_err());
        assert!(parse_command(&tokens("modules bogus")).is_err());
    }

    #[test]
    fn parse_command_history_subcommands() {
        assert!(matches!(
            parse_command(&tokens("history")).unwrap().command,
            CommandType::History(HistoryCommand::List)
        ));
        assert!(matches!(
            parse_command(&tokens("history clear")).unwrap().command,
            CommandType::History(HistoryCommand::Clear)
        ));
        assert!(matches!(
            parse_command(&tokens("history delete 3")).unwrap().command,
            CommandType::History(HistoryCommand::Delete(3))
        ));
        assert!(parse_command(&tokens("history delete")).is_err());
        assert!(parse_command(&tokens("history delete x")).is_err());
        assert!(parse_command(&tokens("history bogus")).is_err());
    }

    fn parameter(name: &str, content_type: &str, default_value: &str) -> QueryParameter {
        QueryParameter {
            name: name.into(),
            default_value: default_value.into(),
            required: false,
            repeatable: false,
            content_type: content_type.into(),
            short_description: String::new(),
            long_description: String::new(),
        }
    }

    fn sample_help() -> QueryHelp {
        QueryHelp {
            name: "check_cpu".into(),
            keyword_source: "check_cpu".into(),
            parameters: vec![
                parameter("warning", "string", "none"),
                parameter("warn-on-error", "string", "none"),
                parameter("show-all", "bool", ""),
                parameter("critical", "string", "none"),
            ],
            fields: vec![
                // The agent leaves a keyword's summary empty and puts the text
                // in the long description, which is what `summary` reaches for.
                field("free", "", "Free disk space"),
                field("used", "Used space", "Used disk space on the drive"),
                field("convert_bytes()", "", "Convert a byte value."),
            ],
        }
    }

    fn field(name: &str, short: &str, long: &str) -> QueryField {
        QueryField {
            name: name.into(),
            short_description: short.into(),
            long_description: long.into(),
        }
    }

    /// Suggestions with nothing to say about them, as an agent that describes
    /// nothing leaves them.
    fn suggestions(texts: &[&str]) -> Vec<Suggestion> {
        texts.iter().copied().map(Suggestion::bare).collect()
    }

    /// An input that already knows `check_cpu` and what it accepts.
    fn input_with_help() -> CommandInput<'static> {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_cpu".into(), "check_drivesize".into()]);
        input.on_query_help("check_cpu", Some(sample_help()));
        input
    }

    #[test]
    fn command_being_argued_waits_for_the_space_after_the_name() {
        assert_eq!(command_being_argued("check_cpu"), None);
        assert_eq!(command_being_argued("check_cpu "), Some("check_cpu"));
        assert_eq!(command_being_argued("check_cpu warn"), Some("check_cpu"));
        // `query <name>` names the query in the second word.
        assert_eq!(command_being_argued("query check_cpu"), None);
        assert_eq!(command_being_argued("query check_cpu "), Some("check_cpu"));
        assert_eq!(command_being_argued(""), None);
    }

    #[test]
    fn command_being_argued_survives_leading_and_multibyte_whitespace() {
        // Leading whitespace used to be counted from the wrong place: the slice
        // landed inside the word and reported "still typing the name", so help
        // was never fetched for an indented line.
        assert_eq!(command_being_argued("  check_cpu "), Some("check_cpu"));
        assert_eq!(command_being_argued("\tcheck_cpu warn"), Some("check_cpu"));
        assert_eq!(command_being_argued("  check_cpu"), None);
        // And with a first character wider than a byte the same slice landed
        // mid-character, which panics -- from a key handler, with the terminal
        // in raw mode.
        assert_eq!(command_being_argued("  ändern "), Some("ändern"));
        assert_eq!(command_being_argued("ändern"), None);
    }

    #[test]
    fn command_being_argued_reads_the_second_word_and_not_a_letter_of_the_first() {
        // `split_once("e")` found the `e` inside `query`, so this never asked
        // for help for a one-letter command name.
        assert_eq!(command_being_argued("query e "), Some("e"));
        assert_eq!(command_being_argued("query e"), None);
        assert_eq!(command_being_argued("query r "), Some("r"));
    }

    #[test]
    fn last_word_steps_over_a_whitespace_character_not_a_byte() {
        assert_eq!(last_word("check_cpu warn"), "warn");
        assert_eq!(last_word("check_cpu "), "");
        assert_eq!(last_word("check_cpu"), "check_cpu");
        // A non-breaking space is whitespace and is two bytes; stepping one
        // byte lands inside it and panics. This is what a command pasted from
        // a web page looks like.
        assert_eq!(last_word("check_cpu\u{a0}warn"), "warn");
    }

    #[test]
    fn tab_completes_a_whole_option_name_that_is_still_being_typed() {
        let mut input = input_with_help();
        // The word under the cursor is what Tab was asked to finish, so it must
        // not count as already given -- this used to return nothing at all.
        type_text(&mut input, "check_cpu critical");
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(input.get_state().value(), "check_cpu critical=");
    }

    #[test]
    fn tab_completes_a_whole_switch_name_that_is_still_being_typed() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu show-all");
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(input.get_state().value(), "check_cpu show-all ");
    }

    #[test]
    fn help_is_asked_for_a_command_name_typed_in_any_case() {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_cpu".into()]);
        // The prompt accepts any case and so does the agent, so the cache has
        // to as well or `Check_CPU` turns the prompt green and completes
        // nothing.
        type_text(&mut input, "Check_CPU ");
        assert_eq!(input.take_help_request(), Some("Check_CPU".into()));

        input.on_query_help("Check_CPU", Some(sample_help()));
        // Answered under one spelling, found under another.
        type_text(&mut input, "crit");
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(input.get_state().value(), "Check_CPU critical=");
    }

    #[test]
    fn moving_between_two_commands_does_not_ask_about_each_one_again() {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_cpu".into(), "check_disk".into()]);

        type_text(&mut input, "check_cpu ");
        assert_eq!(input.take_help_request(), Some("check_cpu".into()));

        input.set_input("check_disk ");
        input.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(input.take_help_request(), Some("check_disk".into()));

        // Back to the first: already asked, so not asked again.
        input.set_input("check_cpu ");
        input.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(input.take_help_request(), None);
    }

    #[test]
    fn a_failed_ask_is_forgotten_so_the_next_keystroke_tries_again() {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_cpu".into()]);

        type_text(&mut input, "check_cpu ");
        assert_eq!(input.take_help_request(), Some("check_cpu".into()));

        // A timeout says nothing about the query; remembering it would leave
        // completion dead for the rest of the session.
        input.forget_query_help("check_cpu");
        type_text(&mut input, "w");
        assert_eq!(input.take_help_request(), Some("check_cpu".into()));
    }

    #[test]
    fn shared_prefix_is_as_far_as_every_candidate_agrees() {
        assert_eq!(shared_prefix(&[]), None);
        assert_eq!(shared_prefix(&suggestions(&["only"])), Some("only".into()));
        assert_eq!(
            shared_prefix(&suggestions(&["warning", "warn-on-error"])),
            Some("warn".into())
        );
        assert_eq!(
            shared_prefix(&suggestions(&["alpha", "beta"])),
            Some(String::new())
        );
    }

    #[test]
    fn tab_completes_a_command_name() {
        let mut input = input_with_help();
        type_text(&mut input, "check_d");
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(input.get_state().value(), "check_drivesize");
    }

    #[test]
    fn tab_completes_an_option_and_leaves_the_cursor_after_its_equals() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu crit");
        assert_eq!(input.complete(), Completion::Extended);
        // An option takes a value on the wire, so the `=` comes with it.
        assert_eq!(input.get_state().value(), "check_cpu critical=");
    }

    #[test]
    fn tab_offers_a_bare_switch_without_an_equals() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu show");
        assert_eq!(input.complete(), Completion::Extended);
        // A bool with no default takes no value at all.
        assert_eq!(input.get_state().value(), "check_cpu show-all ");
    }

    #[test]
    fn tab_extends_to_what_the_matches_agree_on() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu w");
        // `warning` and `warn-on-error` agree on `warn` and no further.
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(input.get_state().value(), "check_cpu warn");
        // Pressing again has nothing left to add, so it shows the choice.
        assert_eq!(
            input.complete(),
            Completion::Candidates(suggestions(&["warning=", "warn-on-error="]))
        );
        assert_eq!(input.get_state().value(), "check_cpu warn");
    }

    #[test]
    fn filter_keyword_being_typed_finds_the_expression_the_line_ends_in() {
        // Not in one at all: the command name, and an option being named.
        assert_eq!(filter_keyword_being_typed("check_cpu"), None);
        assert_eq!(filter_keyword_being_typed("check_cpu "), None);
        assert_eq!(filter_keyword_being_typed("check_cpu warn"), None);
        // In one, from the `=` onwards.
        assert_eq!(filter_keyword_being_typed("check_cpu filter="), Some(""));
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=fr"),
            Some("fr")
        );
        // Every option that takes an expression, abbreviations included.
        for option in ["warning", "warn", "critical", "crit", "ok"] {
            assert_eq!(
                filter_keyword_being_typed(&format!("check_cpu {option}=us")),
                Some("us"),
                "{option}"
            );
        }
        // `empty-state` takes a state, not an expression.
        assert_eq!(filter_keyword_being_typed("check_cpu empty-state=cr"), None);
        // An expression holds spaces, so it is quoted -- and until the quote is
        // closed the line is still inside the value.
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=\"free > 10 and us"),
            Some("us")
        );
        // The operator itself is not a keyword, and every keyword may follow it.
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=\"free > "),
            Some("")
        );
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=\"convert_bytes("),
            Some("")
        );
        // Inside a string literal what comes next is the rest of the literal.
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=\"state = 'run"),
            None
        );
        // Closed again, and a keyword can follow.
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=\"state = 'running' and fr"),
            Some("fr")
        );
        // Past the closing quote the word has ended: the next one is an option.
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=\"free > 10\" sho"),
            None
        );
        // Only the first `=` of the word divides it; the rest is expression.
        assert_eq!(
            filter_keyword_being_typed("check_cpu filter=state=='x'"),
            Some("")
        );
    }

    #[test]
    fn tab_completes_a_filter_keyword_inside_a_filter_expression() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu filter=fr");
        assert_eq!(input.complete(), Completion::Extended);
        // The keyword goes in bare: what follows it is an operator, not a value.
        assert_eq!(input.get_state().value(), "check_cpu filter=free");
    }

    #[test]
    fn tab_keeps_a_filter_functions_parentheses() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu warning=conv");
        assert_eq!(input.complete(), Completion::Extended);
        // The `()` is what tells a function from a variable, so it is part of
        // the name the registry gave.
        assert_eq!(
            input.get_state().value(),
            "check_cpu warning=convert_bytes()"
        );
    }

    #[test]
    fn tab_right_after_the_equals_offers_every_keyword_with_what_it_means() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu filter=");
        assert_eq!(
            input.complete(),
            Completion::Candidates(vec![
                // The agent left the summary empty for these two, so the first
                // line of the long description stands in.
                Suggestion::described("free", "Free disk space".into()),
                Suggestion::described("used", "Used space".into()),
                Suggestion::described("convert_bytes()", "Convert a byte value.".into()),
            ])
        );
        // Showing the choice does not change the line.
        assert_eq!(input.get_state().value(), "check_cpu filter=");
    }

    #[test]
    fn tab_completes_a_keyword_after_an_operator_in_a_quoted_expression() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu filter=\"free > 10 and us");
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(
            input.get_state().value(),
            "check_cpu filter=\"free > 10 and used"
        );
    }

    #[test]
    fn tab_offers_nothing_inside_a_string_literal() {
        let mut input = input_with_help();
        // `us` here is the start of a value the check compares against, and the
        // keywords are not candidates for it.
        type_text(&mut input, "check_cpu filter=\"state = 'us");
        assert_eq!(input.complete(), Completion::Nothing);
        assert_eq!(input.get_state().value(), "check_cpu filter=\"state = 'us");
    }

    #[test]
    fn tab_is_back_to_options_once_the_filter_value_is_over() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu filter=\"free > 10\" show");
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(
            input.get_state().value(),
            "check_cpu filter=\"free > 10\" show-all "
        );
    }

    #[test]
    fn a_keyword_is_offered_again_although_it_is_already_in_the_expression() {
        let mut input = input_with_help();
        // Unlike an option, a keyword may be named twice in one expression.
        type_text(&mut input, "check_cpu filter=\"free > 10 and fr");
        assert_eq!(input.complete(), Completion::Extended);
        assert_eq!(
            input.get_state().value(),
            "check_cpu filter=\"free > 10 and free"
        );
    }

    #[test]
    fn a_check_that_is_not_filter_based_offers_no_keywords() {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_ok".into()]);
        // An empty `fields` list is how the agent says the check is not filter
        // based, and there is then nothing to offer inside `filter=`.
        input.on_query_help(
            "check_ok",
            Some(QueryHelp {
                name: "check_ok".into(),
                keyword_source: "check_ok".into(),
                parameters: vec![parameter("filter", "string", "none")],
                fields: vec![],
            }),
        );
        type_text(&mut input, "check_ok filter=fr");
        assert_eq!(input.complete(), Completion::Nothing);
        assert_eq!(input.get_state().value(), "check_ok filter=fr");
    }

    #[test]
    fn summary_falls_back_to_the_first_line_of_the_long_description() {
        assert_eq!(
            summary("Used space", "Used disk space\nand more"),
            "Used space"
        );
        assert_eq!(
            summary("", "Free disk space\nsecond line"),
            "Free disk space"
        );
        assert_eq!(summary("", ""), "");
    }

    #[test]
    fn candidate_lines_show_names_alone_until_there_is_something_to_say() {
        assert!(candidate_lines(&[]).is_empty());
        // Nothing described: one line, as this always did.
        assert_eq!(
            candidate_lines(&suggestions(&["warning=", "warn-on-error="])),
            vec!["warning=  warn-on-error=".to_string()]
        );
        // Described: a line each, names aligned so the summaries line up.
        assert_eq!(
            candidate_lines(&[
                Suggestion::described("free", "Free disk space".into()),
                Suggestion::described("convert_bytes()", String::new()),
            ]),
            vec![
                "  free             Free disk space".to_string(),
                "  convert_bytes()".to_string(),
            ]
        );
    }

    #[test]
    fn tab_does_not_offer_an_option_that_is_already_on_the_line() {
        let mut input = input_with_help();
        type_text(&mut input, "check_cpu critical=5 crit");
        assert_eq!(input.complete(), Completion::Nothing);
        assert_eq!(input.get_state().value(), "check_cpu critical=5 crit");
    }

    #[test]
    fn tab_does_nothing_for_a_query_the_agent_could_not_describe() {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_cpu".into()]);
        input.on_query_help("check_cpu", None);
        type_text(&mut input, "check_cpu w");
        assert_eq!(input.complete(), Completion::Nothing);
        assert_eq!(input.get_state().value(), "check_cpu w");
    }

    #[test]
    fn help_is_asked_for_once_the_arguments_start_and_only_once() {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_cpu".into()]);

        type_text(&mut input, "check_cpu");
        assert_eq!(input.take_help_request(), None, "still typing the name");

        type_text(&mut input, " ");
        assert_eq!(input.take_help_request(), Some("check_cpu".into()));

        // Taken once: the request is not repeated on every later keystroke.
        type_text(&mut input, "war");
        assert_eq!(input.take_help_request(), None);
    }

    #[test]
    fn help_is_not_asked_for_something_that_is_not_a_query() {
        let mut input = CommandInput::new(vec![]);
        input.update_commands(vec!["check_cpu".into()]);
        type_text(&mut input, "modules list");
        assert_eq!(input.take_help_request(), None);
    }

    fn type_text(input: &mut CommandInput, text: &str) {
        for c in text.chars() {
            input.handle_key_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn press(input: &mut CommandInput, code: KeyCode) {
        input.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn value(input: &mut CommandInput) -> String {
        input.get_state().value().to_string()
    }

    #[test]
    fn valid_command_is_recorded_in_history_and_clears_input() {
        let mut input = CommandInput::new(vec![]);
        type_text(&mut input, "ping");
        assert!(input.has_value());
        assert!(input.get_command().is_ok());
        assert_eq!(input.get_history(), vec!["ping".to_string()]);
        assert!(!input.has_value());
    }

    #[test]
    fn invalid_command_is_not_recorded_and_keeps_input() {
        let mut input = CommandInput::new(vec![]);
        type_text(&mut input, "bogus");
        assert!(input.get_command().is_err());
        assert!(input.get_history().is_empty());
        assert_eq!(value(&mut input), "bogus");

        type_text(&mut input, " \"unterminated");
        assert!(input.get_command().is_err());
        assert!(input.get_history().is_empty());
    }

    #[test]
    fn history_is_deduplicated_and_capped() {
        let mut input = CommandInput::new(vec![]);
        for i in 0..(MAX_HISTORY_LENGTH + 5) {
            input.add_history(&format!("cmd{i}"));
        }
        assert_eq!(input.history.len(), MAX_HISTORY_LENGTH);
        assert_eq!(input.history[0], "cmd5");

        input.add_history("cmd7");
        assert_eq!(input.history.len(), MAX_HISTORY_LENGTH);
        assert_eq!(input.history.last().unwrap(), "cmd7");
        assert_eq!(input.history.iter().filter(|c| *c == "cmd7").count(), 1);

        input.add_history("   ");
        assert_eq!(input.history.len(), MAX_HISTORY_LENGTH);
    }

    #[test]
    fn history_navigation_restores_draft_and_does_not_record_it() {
        let mut input = CommandInput::new(vec!["first".into(), "second".into()]);
        type_text(&mut input, "dra");

        press(&mut input, KeyCode::Up);
        assert_eq!(value(&mut input), "second");
        press(&mut input, KeyCode::Up);
        assert_eq!(value(&mut input), "first");
        // Going past the oldest entry stays on the oldest entry.
        press(&mut input, KeyCode::Up);
        assert_eq!(value(&mut input), "first");

        press(&mut input, KeyCode::Down);
        assert_eq!(value(&mut input), "second");
        press(&mut input, KeyCode::Down);
        assert_eq!(value(&mut input), "dra");
        // Down with nothing selected is a no-op.
        press(&mut input, KeyCode::Down);
        assert_eq!(value(&mut input), "dra");

        assert_eq!(
            input.get_history(),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn history_navigation_with_empty_history_is_a_noop() {
        let mut input = CommandInput::new(vec![]);
        type_text(&mut input, "abc");
        press(&mut input, KeyCode::Up);
        press(&mut input, KeyCode::Down);
        assert_eq!(value(&mut input), "abc");
    }

    #[test]
    fn release_key_events_are_ignored() {
        let mut input = CommandInput::new(vec![]);
        let mut event = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        event.kind = KeyEventKind::Release;
        input.handle_key_event(event);
        assert!(!input.has_value());
    }

    #[test]
    fn history_delete_out_of_range_does_not_panic() {
        let mut input = CommandInput::new(vec!["one".into(), "two".into()]);
        let result = input.handle_history(HistoryCommand::Delete(5));
        assert!(result[0].contains("Invalid history index"), "{result:?}");
        assert_eq!(
            input.get_history(),
            vec!["one".to_string(), "two".to_string()]
        );

        let result = input.handle_history(HistoryCommand::Delete(0));
        assert_eq!(result, vec!["History deleted".to_string()]);
        assert_eq!(input.get_history(), vec!["two".to_string()]);
    }
}
