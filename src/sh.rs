//! `sh` applet — a deliberately minimal POSIX-ish shell (NOT bash), enough to
//! run the `sh -c "..."` command lines that workloads (and the CI smoke test)
//! use, and to serve as the default interactive `attach` shell. Replaces
//! busybox `ash`.
//!
//! Supports: `;` sequences, `&&`/`||` and-or lists, `|` pipelines, `>`/`>>`/`<`
//! redirection, single/double quoting, `$VAR`/`${VAR}`/`$?` expansion, a few
//! builtins (cd, export, unset, echo, pwd, exit, true/false/:), `NAME=value`
//! assignments, and external commands via PATH. Deliberately omits loops,
//! conditionals, subshells, globbing, job control — workloads needing those
//! ship their own interpreter.

use std::collections::HashMap;
use std::io::{BufRead, Write};

pub fn main(args: Vec<String>) -> i32 {
    let mut sh = Shell::new();
    match args.first().map(String::as_str) {
        None => sh.run_interactive(),
        Some("-c") => {
            let script = args.get(1).cloned().unwrap_or_default();
            sh.run_script(&script)
        }
        Some("-s") => sh.run_interactive(),
        Some(path) => match std::fs::read_to_string(path) {
            Ok(body) => sh.run_script(&body),
            Err(e) => {
                eprintln!("sh: cannot open {path}: {e}");
                127
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Lexing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Lit(String), // not subject to $-expansion (single-quoted / escaped)
    Exp(String), // subject to $-expansion (unquoted / double-quoted)
}

type Word = Vec<Seg>;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(Word),
    Semi,
    And,
    Or,
    Pipe,
    Redir { append: bool }, // > or >>
    RedirIn,                // <
}

#[derive(Default)]
struct WordBuf {
    segs: Vec<Seg>,
    buf: String,
    lit: bool,
    active: bool,
}

impl WordBuf {
    fn push(&mut self, c: char, lit: bool) {
        self.active = true;
        if !self.buf.is_empty() && self.lit != lit {
            self.flush();
        }
        self.lit = lit;
        self.buf.push(c);
    }
    fn start(&mut self) {
        self.active = true;
    }
    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let s = std::mem::take(&mut self.buf);
            self.segs
                .push(if self.lit { Seg::Lit(s) } else { Seg::Exp(s) });
        }
    }
    fn take(&mut self) -> Option<Word> {
        if !self.active {
            return None;
        }
        self.flush();
        self.active = false;
        Some(std::mem::take(&mut self.segs))
    }
}

fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut wb = WordBuf::default();
    let mut chars = input.chars().peekable();

    macro_rules! flush_word {
        () => {
            if let Some(w) = wb.take() {
                toks.push(Tok::Word(w));
            }
        };
    }

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => flush_word!(),
            '\n' | '\r' => {
                flush_word!();
                toks.push(Tok::Semi);
            }
            '#' if !wb.active => {
                while let Some(&n) = chars.peek() {
                    if n == '\n' {
                        break;
                    }
                    chars.next();
                }
            }
            ';' => {
                flush_word!();
                toks.push(Tok::Semi);
            }
            '|' => {
                flush_word!();
                if chars.peek() == Some(&'|') {
                    chars.next();
                    toks.push(Tok::Or);
                } else {
                    toks.push(Tok::Pipe);
                }
            }
            '&' => {
                flush_word!();
                if chars.peek() == Some(&'&') {
                    chars.next();
                    toks.push(Tok::And);
                } else {
                    // background (&) unsupported — treat as a sequence point.
                    toks.push(Tok::Semi);
                }
            }
            '>' => {
                flush_word!();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    toks.push(Tok::Redir { append: true });
                } else {
                    toks.push(Tok::Redir { append: false });
                }
            }
            '<' => {
                flush_word!();
                toks.push(Tok::RedirIn);
            }
            '\'' => {
                wb.start();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => wb.push(ch, true),
                        None => return Err("unterminated single quote".into()),
                    }
                }
            }
            '"' => {
                wb.start();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(n @ ('"' | '\\' | '$' | '`')) => wb.push(n, true),
                            Some(n) => {
                                wb.push('\\', true);
                                wb.push(n, true);
                            }
                            None => return Err("unterminated double quote".into()),
                        },
                        Some(ch) => wb.push(ch, false), // $ still expands
                        None => return Err("unterminated double quote".into()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {} // line continuation
                Some(n) => wb.push(n, true),
                None => return Err("trailing backslash".into()),
            },
            other => wb.push(other, false),
        }
    }
    if let Some(w) = wb.take() {
        toks.push(Tok::Word(w));
    }
    Ok(toks)
}

// ---------------------------------------------------------------------------
// Parsing: tokens -> pipelines joined by connectors
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
struct Command {
    argv: Vec<Word>,
    redir_out: Option<(Word, bool)>, // (target, append)
    redir_in: Option<Word>,
}

impl Command {
    fn is_empty(&self) -> bool {
        self.argv.is_empty() && self.redir_out.is_none() && self.redir_in.is_none()
    }
}

type Pipeline = Vec<Command>;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Conn {
    Semi,
    And,
    Or,
}

fn parse(toks: &[Tok]) -> Result<(Vec<Pipeline>, Vec<Conn>), String> {
    let mut pipelines: Vec<Pipeline> = Vec::new();
    let mut conns: Vec<Conn> = Vec::new();
    let mut pipeline: Pipeline = Vec::new();
    let mut cmd = Command::default();
    let mut it = toks.iter().peekable();

    // Close the in-progress command + pipeline, recording `conn` as the
    // connector that follows it. Drops empty pipelines (blank statements).
    let close = |pipeline: &mut Pipeline,
                 cmd: &mut Command,
                 pipelines: &mut Vec<Pipeline>,
                 conns: &mut Vec<Conn>,
                 conn: Option<Conn>| {
        pipeline.push(std::mem::take(cmd));
        let nonempty = pipeline.iter().any(|c| !c.is_empty());
        if nonempty {
            pipelines.push(std::mem::take(pipeline));
            if let Some(c) = conn {
                conns.push(c);
            }
        } else {
            pipeline.clear();
        }
    };

    while let Some(t) = it.next() {
        match t {
            Tok::Word(w) => cmd.argv.push(w.clone()),
            Tok::Redir { append } => match it.next() {
                Some(Tok::Word(w)) => cmd.redir_out = Some((w.clone(), *append)),
                _ => return Err("expected filename after redirection".into()),
            },
            Tok::RedirIn => match it.next() {
                Some(Tok::Word(w)) => cmd.redir_in = Some(w.clone()),
                _ => return Err("expected filename after <".into()),
            },
            Tok::Pipe => pipeline.push(std::mem::take(&mut cmd)),
            Tok::Semi => close(
                &mut pipeline,
                &mut cmd,
                &mut pipelines,
                &mut conns,
                Some(Conn::Semi),
            ),
            Tok::And => close(
                &mut pipeline,
                &mut cmd,
                &mut pipelines,
                &mut conns,
                Some(Conn::And),
            ),
            Tok::Or => close(
                &mut pipeline,
                &mut cmd,
                &mut pipelines,
                &mut conns,
                Some(Conn::Or),
            ),
        }
    }
    close(&mut pipeline, &mut cmd, &mut pipelines, &mut conns, None);

    // A trailing connector (e.g. `a;`) leaves one extra connector — drop it so
    // conns.len() == pipelines.len().saturating_sub(1).
    while conns.len() >= pipelines.len() && !conns.is_empty() {
        conns.pop();
    }
    Ok((pipelines, conns))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

struct Shell {
    vars: HashMap<String, String>,
    last: i32,
}

impl Shell {
    fn new() -> Self {
        Shell {
            vars: HashMap::new(),
            last: 0,
        }
    }

    fn run_interactive(&mut self) -> i32 {
        let stdin = std::io::stdin();
        let interactive = unsafe { libc::isatty(0) } == 1;
        loop {
            if interactive {
                print!("$ ");
                let _ = std::io::stdout().flush();
            }
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    self.run_script(&line);
                }
                Err(_) => break,
            }
        }
        self.last
    }

    fn run_script(&mut self, src: &str) -> i32 {
        let toks = match tokenize(src) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("sh: {e}");
                self.last = 2;
                return 2;
            }
        };
        let (pipelines, conns) = match parse(&toks) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("sh: {e}");
                self.last = 2;
                return 2;
            }
        };
        for idx in 0..pipelines.len() {
            let should = if idx == 0 {
                true
            } else {
                match conns[idx - 1] {
                    Conn::Semi => true,
                    Conn::And => self.last == 0,
                    Conn::Or => self.last != 0,
                }
            };
            if should {
                self.last = self.exec_pipeline(&pipelines[idx]);
            }
        }
        self.last
    }

    fn exec_pipeline(&mut self, pipeline: &[Command]) -> i32 {
        if pipeline.len() == 1 {
            return self.exec_command(&pipeline[0]);
        }
        self.exec_real_pipeline(pipeline)
    }

    fn exec_command(&mut self, cmd: &Command) -> i32 {
        // Leading NAME=value assignments precede the command word.
        let mut argv: Vec<String> = Vec::new();
        let mut assigns: Vec<(String, String)> = Vec::new();
        let mut seen_cmd = false;
        for w in &cmd.argv {
            let s = self.expand_word(w);
            if !seen_cmd {
                if let Some((k, v)) = split_assignment(&s) {
                    assigns.push((k, v));
                    continue;
                }
                seen_cmd = true;
            }
            argv.push(s);
        }

        if argv.is_empty() {
            // Pure assignments set shell vars; a bare redirection still applies.
            for (k, v) in assigns {
                self.vars.insert(k, v);
            }
            if let Some((w, append)) = &cmd.redir_out {
                let path = self.expand_word(w);
                if let Err(e) = open_out(&path, *append) {
                    eprintln!("sh: {path}: {e}");
                    return 1;
                }
            }
            return 0;
        }

        match argv[0].as_str() {
            "cd" | "exit" | "export" | "unset" | "echo" | "pwd" | "true" | "false" | ":"
            | "set" | "read" => self.run_builtin(&argv, cmd),
            _ => self.spawn_external(&argv, cmd, &assigns),
        }
    }

    fn run_builtin(&mut self, argv: &[String], cmd: &Command) -> i32 {
        match argv[0].as_str() {
            ":" | "true" => 0,
            "false" => 1,
            "set" | "read" => 0, // accepted, ignored
            "pwd" => {
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.emit(cmd, &format!("{cwd}\n"))
            }
            "cd" => {
                let dir = argv
                    .get(1)
                    .cloned()
                    .or_else(|| self.get_var_opt("HOME"))
                    .unwrap_or_else(|| "/".into());
                match std::env::set_current_dir(&dir) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("cd: {dir}: {e}");
                        1
                    }
                }
            }
            "exit" => {
                let code = argv
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(self.last);
                std::process::exit(code);
            }
            "export" => {
                for a in &argv[1..] {
                    if let Some((k, v)) = split_assignment(a) {
                        std::env::set_var(&k, &v);
                        self.vars.insert(k, v);
                    } else if let Some(v) = self.vars.get(a) {
                        std::env::set_var(a, v);
                    }
                }
                0
            }
            "unset" => {
                for a in &argv[1..] {
                    self.vars.remove(a);
                    std::env::remove_var(a);
                }
                0
            }
            "echo" => {
                let mut rest = &argv[1..];
                let mut newline = true;
                if rest.first().map(String::as_str) == Some("-n") {
                    newline = false;
                    rest = &rest[1..];
                }
                let mut out = rest.join(" ");
                if newline {
                    out.push('\n');
                }
                self.emit(cmd, &out)
            }
            _ => 0,
        }
    }

    /// Write builtin output honoring a `>`/`>>` redirection, else stdout.
    fn emit(&self, cmd: &Command, text: &str) -> i32 {
        if let Some((w, append)) = &cmd.redir_out {
            let path = self.expand_word(w);
            match open_out(&path, *append) {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(text.as_bytes()) {
                        eprintln!("sh: {path}: {e}");
                        return 1;
                    }
                    0
                }
                Err(e) => {
                    eprintln!("sh: {path}: {e}");
                    1
                }
            }
        } else {
            print!("{text}");
            let _ = std::io::stdout().flush();
            0
        }
    }

    fn spawn_external(
        &mut self,
        argv: &[String],
        cmd: &Command,
        assigns: &[(String, String)],
    ) -> i32 {
        let mut c = std::process::Command::new(&argv[0]);
        c.args(&argv[1..]);
        for (k, v) in assigns {
            c.env(k, v);
        }
        if let Some((w, append)) = &cmd.redir_out {
            let path = self.expand_word(w);
            match open_out(&path, *append) {
                Ok(f) => {
                    c.stdout(f);
                }
                Err(e) => {
                    eprintln!("sh: {path}: {e}");
                    return 1;
                }
            }
        }
        if let Some(w) = &cmd.redir_in {
            let path = self.expand_word(w);
            match std::fs::File::open(&path) {
                Ok(f) => {
                    c.stdin(f);
                }
                Err(e) => {
                    eprintln!("sh: {path}: {e}");
                    return 1;
                }
            }
        }
        match c.status() {
            Ok(s) => s.code().unwrap_or(if s.success() { 0 } else { 1 }),
            Err(e) => {
                eprintln!("sh: {}: {e}", argv[0]);
                127
            }
        }
    }

    fn exec_real_pipeline(&mut self, pipeline: &[Command]) -> i32 {
        use std::process::{Command as PCommand, Stdio};
        let mut children = Vec::new();
        let mut prev_out: Option<std::process::ChildStdout> = None;
        let n = pipeline.len();
        for (i, cmd) in pipeline.iter().enumerate() {
            let argv: Vec<String> = cmd.argv.iter().map(|w| self.expand_word(w)).collect();
            if argv.is_empty() {
                eprintln!("sh: empty command in pipeline");
                return 2;
            }
            let mut c = PCommand::new(&argv[0]);
            c.args(&argv[1..]);
            if let Some(out) = prev_out.take() {
                c.stdin(out);
            } else if let Some(w) = &cmd.redir_in {
                let path = self.expand_word(w);
                match std::fs::File::open(&path) {
                    Ok(f) => {
                        c.stdin(f);
                    }
                    Err(e) => {
                        eprintln!("sh: {path}: {e}");
                        return 1;
                    }
                }
            }
            if i == n - 1 {
                if let Some((w, append)) = &cmd.redir_out {
                    let path = self.expand_word(w);
                    match open_out(&path, *append) {
                        Ok(f) => {
                            c.stdout(f);
                        }
                        Err(e) => {
                            eprintln!("sh: {path}: {e}");
                            return 1;
                        }
                    }
                }
            } else {
                c.stdout(Stdio::piped());
            }
            let mut child = match c.spawn() {
                Ok(ch) => ch,
                Err(e) => {
                    eprintln!("sh: {}: {e}", argv[0]);
                    return 127;
                }
            };
            if i != n - 1 {
                prev_out = child.stdout.take();
            }
            children.push(child);
        }
        let mut status = 0;
        for (i, mut ch) in children.into_iter().enumerate() {
            let s = ch.wait();
            if i == n - 1 {
                status = s.ok().and_then(|s| s.code()).unwrap_or(1);
            }
        }
        status
    }

    // -- variable expansion --

    fn expand_word(&self, word: &Word) -> String {
        let mut out = String::new();
        for seg in word {
            match seg {
                Seg::Lit(s) => out.push_str(s),
                Seg::Exp(s) => out.push_str(&self.expand_dollars(s)),
            }
        }
        out
    }

    fn expand_dollars(&self, s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '$' {
                out.push(c);
                continue;
            }
            match chars.peek() {
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if n == '}' {
                            break;
                        }
                        name.push(n);
                    }
                    out.push_str(&self.get_var(&name));
                }
                Some('?') => {
                    chars.next();
                    out.push_str(&self.last.to_string());
                }
                Some(&n) if n.is_alphanumeric() || n == '_' => {
                    let mut name = String::new();
                    while let Some(&n) = chars.peek() {
                        if n.is_alphanumeric() || n == '_' {
                            name.push(n);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push_str(&self.get_var(&name));
                }
                _ => out.push('$'),
            }
        }
        out
    }

    fn get_var(&self, name: &str) -> String {
        self.get_var_opt(name).unwrap_or_default()
    }

    fn get_var_opt(&self, name: &str) -> Option<String> {
        self.vars
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
    }
}

fn split_assignment(s: &str) -> Option<(String, String)> {
    let eq = s.find('=')?;
    let (name, rest) = s.split_at(eq);
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((name.to_string(), rest[1..].to_string()))
}

fn open_out(path: &str, append: bool) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(toks: &[Tok]) -> Vec<String> {
        // Render each Word token by concatenating its raw segment text.
        toks.iter()
            .filter_map(|t| match t {
                Tok::Word(w) => Some(
                    w.iter()
                        .map(|s| match s {
                            Seg::Lit(x) | Seg::Exp(x) => x.clone(),
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tokenize_basic() {
        let t = tokenize("echo hi there").unwrap();
        assert_eq!(words(&t), vec!["echo", "hi", "there"]);
    }

    #[test]
    fn tokenize_operators() {
        let t = tokenize("a && b || c | d ; e").unwrap();
        assert!(t.contains(&Tok::And));
        assert!(t.contains(&Tok::Or));
        assert!(t.contains(&Tok::Pipe));
        assert!(t.contains(&Tok::Semi));
    }

    #[test]
    fn quotes_and_redirection() {
        let t = tokenize("echo 'a b' \"c d\" > /tmp/x").unwrap();
        assert!(t.iter().any(|x| matches!(x, Tok::Redir { append: false })));
        assert_eq!(words(&t), vec!["echo", "a b", "c d", "/tmp/x"]);
    }

    #[test]
    fn expansion_single_vs_double() {
        let mut sh = Shell::new();
        sh.vars.insert("X".into(), "5".into());
        let dq = tokenize("\"$X\"").unwrap();
        let sq = tokenize("'$X'").unwrap();
        if let Tok::Word(w) = &dq[0] {
            assert_eq!(sh.expand_word(w), "5");
        }
        if let Tok::Word(w) = &sq[0] {
            assert_eq!(sh.expand_word(w), "$X");
        }
    }

    #[test]
    fn redirection_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("out");
        let mut sh = Shell::new();
        let code = sh.run_script(&format!("echo ok > {}", f.display()));
        assert_eq!(code, 0);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "ok\n");
    }

    #[test]
    fn and_or_short_circuit() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("o");
        let mut sh = Shell::new();
        // false && echo A  -> A not written;  true || echo B -> B not written;
        // true && echo C   -> C written.
        sh.run_script(&format!(
            "false && echo A > {0}; true || echo B > {0}; true && echo C > {0}",
            f.display()
        ));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "C\n");
    }

    #[test]
    fn assignment_then_expand() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("o");
        let mut sh = Shell::new();
        sh.run_script(&format!("X=hello; echo $X > {}", f.display()));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello\n");
    }

    #[test]
    fn status_var() {
        let mut sh = Shell::new();
        sh.run_script("false");
        assert_eq!(sh.last, 1);
        sh.run_script("true");
        assert_eq!(sh.last, 0);
    }
}
