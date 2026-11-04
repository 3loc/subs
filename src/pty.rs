//! Run a terminal program on a private pseudo-terminal and read its screen back.
//!
//! `claude setup-token` is a full-screen TUI: it wants a terminal, and it draws
//! with cursor moves rather than plain lines. Its output never reaches ours;
//! callers pick the few things they need off the reconstructed screen.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub struct Term {
    child: Child,
    input: File,
    /// Output as it arrives; disconnected once the program's side is closed.
    pub output: Receiver<Vec<u8>>,
}

fn openpty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
    let (mut m, mut s) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: valid out-pointers; on success both descriptors are new and ours.
    unsafe {
        if libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            // *const on Linux, *mut on macOS
            std::ptr::addr_of_mut!(size),
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        let (m, s) = (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s));
        for fd in [&m, &s] {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok((m, s))
    }
}

impl Term {
    /// Start `cmd` in its own session with the terminal as its controlling tty,
    /// so it cannot reach the user's terminal through /dev/tty either.
    pub fn spawn(mut cmd: Command, cols: u16, rows: u16) -> io::Result<Term> {
        let (master, slave) = openpty(cols, rows)?;
        cmd.stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        // Our copies of the program's side: reads only end once these are gone.
        drop(cmd);
        let input = File::from(master);
        let mut reader = input.try_clone()?;
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16384];
            loop {
                match reader.read(&mut buf) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    // EIO once the program's side closes
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Term {
            child,
            input,
            output,
        })
    }

    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.input.write_all(bytes)
    }
}

impl Drop for Term {
    /// End the whole session: the program may have started helpers of its own.
    fn drop(&mut self) {
        let group = -(self.child.id() as libc::pid_t);
        // SAFETY: plain signal delivery to the session we created.
        unsafe { libc::kill(group, libc::SIGTERM) };
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe { libc::kill(group, libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ground,
    Esc,
    Csi,
    /// OSC, DCS and friends: skipped up to BEL or ST.
    Str,
    StrEsc,
    /// ESC ( B and the like take one more byte.
    Charset,
}

/// Just enough of a terminal to read text back: printing, cursor movement and
/// erasing. Lines scrolled off the top are kept, so nothing drawn is lost.
pub struct Screen {
    lines: Vec<Vec<char>>,
    rows: usize,
    top: usize,
    row: usize,
    col: usize,
    saved: (usize, usize),
    mode: Mode,
    params: String,
    pending: Vec<u8>,
}

impl Screen {
    pub fn new(rows: usize) -> Self {
        Screen {
            lines: vec![vec![]],
            rows: rows.max(1),
            top: 0,
            row: 0,
            col: 0,
            saved: (0, 0),
            mode: Mode::Ground,
            params: String::new(),
            pending: vec![],
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let (mut done, mut text) = (0, String::new());
        loop {
            match std::str::from_utf8(&self.pending[done..]) {
                Ok(s) => {
                    text.push_str(s);
                    done = self.pending.len();
                    break;
                }
                Err(e) => {
                    let end = done + e.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&self.pending[done..end]).unwrap_or_default(),
                    );
                    match e.error_len() {
                        // a character split across reads waits for the rest
                        None => {
                            done = end;
                            break;
                        }
                        Some(n) => {
                            text.push('\u{fffd}');
                            done = end + n;
                        }
                    }
                }
            }
        }
        self.pending.drain(..done);
        for c in text.chars() {
            self.put(c);
        }
    }

    fn put(&mut self, c: char) {
        match self.mode {
            Mode::Esc => {
                self.mode = Mode::Ground;
                match c {
                    '[' => {
                        self.mode = Mode::Csi;
                        self.params.clear();
                    }
                    ']' | 'P' | '_' | '^' | 'X' => self.mode = Mode::Str,
                    '(' | ')' | '*' | '+' => self.mode = Mode::Charset,
                    '7' => self.saved = (self.row - self.top, self.col),
                    '8' => {
                        self.row = self.top + self.saved.0;
                        self.col = self.saved.1;
                        self.grow();
                    }
                    _ => {}
                }
            }
            Mode::Charset => self.mode = Mode::Ground,
            Mode::Str => match c {
                '\x07' | '\u{9c}' => self.mode = Mode::Ground,
                '\x1b' => self.mode = Mode::StrEsc,
                _ => {}
            },
            Mode::StrEsc => self.mode = if c == '\\' { Mode::Ground } else { Mode::Str },
            Mode::Csi if ('\x20'..='\x3f').contains(&c) => self.params.push(c),
            Mode::Csi => {
                self.mode = Mode::Ground;
                self.csi(c);
            }
            Mode::Ground => match c {
                '\x1b' => self.mode = Mode::Esc,
                '\r' => self.col = 0,
                '\n' | '\x0b' | '\x0c' => self.down(1, true),
                '\x08' => self.col = self.col.saturating_sub(1),
                '\t' => self.col = (self.col / 8 + 1) * 8,
                c if c.is_control() => {}
                c => {
                    let line = &mut self.lines[self.row];
                    if line.len() <= self.col {
                        line.resize(self.col + 1, ' ');
                    }
                    line[self.col] = c;
                    self.col += 1;
                }
            },
        }
    }

    fn grow(&mut self) {
        if self.lines.len() <= self.row {
            self.lines.resize(self.row + 1, vec![]);
        }
    }

    fn down(&mut self, n: usize, scroll: bool) {
        self.row += n;
        let bottom = self.top + self.rows - 1;
        if self.row > bottom {
            if scroll {
                self.top = self.row + 1 - self.rows;
            } else {
                self.row = bottom;
            }
        }
        self.grow();
    }

    fn csi(&mut self, fin: char) {
        if self.params.starts_with(['?', '>', '<', '=']) {
            return;
        }
        let n: Vec<usize> = self
            .params
            .split(';')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        let arg = |i: usize| n.get(i).copied().unwrap_or(0);
        let count = arg(0).max(1);
        match fin {
            'A' => self.row = self.row.saturating_sub(count).max(self.top),
            'B' => self.down(count, false),
            'C' => self.col += count,
            'D' => self.col = self.col.saturating_sub(count),
            'E' => {
                self.down(count, false);
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(count).max(self.top);
                self.col = 0;
            }
            'G' | '`' => self.col = count - 1,
            'd' => {
                self.row = self.top + count.min(self.rows) - 1;
                self.grow();
            }
            'H' | 'f' => {
                self.row = self.top + arg(0).clamp(1, self.rows) - 1;
                self.col = arg(1).max(1) - 1;
                self.grow();
            }
            'K' => {
                let col = self.col;
                let line = &mut self.lines[self.row];
                match arg(0) {
                    0 => line.truncate(col),
                    1 => line.iter_mut().take(col + 1).for_each(|c| *c = ' '),
                    _ => line.clear(),
                }
            }
            'J' => {
                let (row, col) = (self.row, self.col);
                match arg(0) {
                    0 => {
                        self.lines[row].truncate(col);
                        self.lines.truncate(row + 1);
                    }
                    1 => {
                        for l in &mut self.lines[self.top..row] {
                            l.clear();
                        }
                        let line = &mut self.lines[row];
                        line.iter_mut().take(col + 1).for_each(|c| *c = ' ');
                    }
                    _ => {
                        for l in &mut self.lines[self.top..] {
                            l.clear();
                        }
                    }
                }
            }
            'X' => {
                let (col, line) = (self.col, &mut self.lines[self.row]);
                line.iter_mut().skip(col).take(count).for_each(|c| *c = ' ');
            }
            'P' => {
                let (col, line) = (self.col, &mut self.lines[self.row]);
                if col < line.len() {
                    let end = (col + count).min(line.len());
                    line.drain(col..end);
                }
            }
            '@' => {
                let (col, line) = (self.col, &mut self.lines[self.row]);
                if col < line.len() {
                    line.splice(col..col, std::iter::repeat_n(' ', count));
                }
            }
            _ => {}
        }
    }

    /// Every line drawn so far, scrolled-off ones included.
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>().trim_end().to_string())
            .collect()
    }

    /// The cursor as (line index into `lines()`, column).
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(chunks: &[&[u8]]) -> Screen {
        let mut s = Screen::new(10);
        for c in chunks {
            s.feed(c);
        }
        s
    }

    #[test]
    fn cursor_drawn_words_osc_links_and_colours_read_back_as_text() {
        let s = screen(&[
            b"\x1b7\x1b8\x1b[2GPaste\x1b[8Gcode\x1b[13Ghere\r\r\n",
            b"\x1b]8;id=x;https://example.invalid/a?b=c\x07\x1b[38;5;246mhttps://example.invalid/a?b=c\x1b[39m\x1b]8;;\x07\r\n",
            b"\x1b(Bdone\x1b[?25l",
        ]);
        assert_eq!(
            s.lines(),
            [" Paste code here", "https://example.invalid/a?b=c", "done"]
        );
        assert_eq!(s.cursor(), (2, 4));
    }

    #[test]
    fn split_utf8_moves_erases_and_redraws_are_applied() {
        let s = screen(&[
            b"\xe2\x9c",
            b"\xbb old line\r\n",
            b"abcdef\x1b[3D\x1b[K",
            b"XYZ\x1b[1A\r\x1b[2K",
            b"new\x1b[1B\r\x1b[1CQ",
        ]);
        assert_eq!(s.lines(), ["new", "aQcXYZ"]);
    }

    #[test]
    fn scrolled_lines_are_kept_and_absolute_moves_are_screen_relative() {
        let mut s = Screen::new(2);
        s.feed(b"one\r\ntwo\r\nthree\x1b[1;1Hx\x1b[2J");
        assert_eq!(s.lines(), ["one", "", ""]);
        let mut s = Screen::new(2);
        s.feed(b"one\r\ntwo\r\nthree\x1b[1;1Hx");
        assert_eq!(s.lines(), ["one", "xwo", "three"]);
    }
}
