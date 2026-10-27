//! C4: stopping a runtime. Cua cannot cancel an action already running
//! (issue #3796, open on 16 Oct 2026): a drag kept sending after its client
//! and daemon were killed, and no mouse-up arrived. So the helper ends the
//! processes, then checks each mouse button itself and posts a release for
//! any still held.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Other,
}

impl Button {
    pub const ALL: [Button; 3] = [Button::Left, Button::Right, Button::Other];
    pub fn name(self) -> &'static str {
        match self {
            Button::Left => "left",
            Button::Right => "right",
            Button::Other => "other",
        }
    }
}

/// The system's mouse, behind a trait so the sequence is tested without one.
pub trait Mouse {
    fn held(&self, button: Button) -> Option<bool>;
    /// Posts a release of `button` where the pointer is.
    fn release(&self, button: Button) -> bool;
}

/// The processes of one runtime.
pub trait Processes {
    /// Ends every process of the runtime: a polite signal, then a kill after
    /// `grace`. True when none is left.
    fn end(&mut self, grace: Duration) -> bool;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct StopReport {
    pub ended: bool,
    pub released: Vec<&'static str>,
    pub warning: Option<String>,
}

pub const GRACE: Duration = Duration::from_millis(800);

/// End the processes first, so nothing presses a button again after the
/// release; then release what is held, and look once more.
pub fn stop(processes: &mut dyn Processes, mouse: &dyn Mouse) -> StopReport {
    let ended = processes.end(GRACE);
    let mut report = StopReport {
        ended,
        ..StopReport::default()
    };
    let mut unknown = false;
    for button in Button::ALL {
        match mouse.held(button) {
            Some(true) => {
                if mouse.release(button) {
                    report.released.push(button.name());
                }
            }
            Some(false) => {}
            None => unknown = true,
        }
    }
    let still: Vec<&str> = Button::ALL
        .iter()
        .filter(|button| mouse.held(**button) == Some(true))
        .map(|button| button.name())
        .collect();
    if !ended {
        report.warning = Some(
            "A computer-use process did not end. Quit MonoCode Computer Use from Activity Monitor."
                .into(),
        );
    } else if !still.is_empty() {
        report.warning = Some(format!(
            "The {} mouse button still reads as held. Click once anywhere to release it.",
            still.join(" and ")
        ));
    } else if unknown {
        report.warning = Some(
            "The mouse buttons could not be read. If one seems stuck, click once anywhere.".into(),
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeMouse {
        held: RefCell<Vec<Button>>,
        sticky: bool,
        readable: bool,
        log: RefCell<Vec<String>>,
    }
    impl Mouse for FakeMouse {
        fn held(&self, button: Button) -> Option<bool> {
            self.readable.then(|| self.held.borrow().contains(&button))
        }
        fn release(&self, button: Button) -> bool {
            self.log
                .borrow_mut()
                .push(format!("release {}", button.name()));
            if !self.sticky {
                self.held.borrow_mut().retain(|b| *b != button);
            }
            true
        }
    }
    struct FakeProcs<'a> {
        ok: bool,
        log: &'a RefCell<Vec<String>>,
    }
    impl Processes for FakeProcs<'_> {
        fn end(&mut self, _grace: Duration) -> bool {
            self.log.borrow_mut().push("end".into());
            self.ok
        }
    }
    fn mouse(held: Vec<Button>) -> FakeMouse {
        FakeMouse {
            held: RefCell::new(held),
            sticky: false,
            readable: true,
            log: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn ends_the_processes_before_releasing_a_held_button() {
        let mouse = mouse(vec![Button::Left]);
        let mut procs = FakeProcs {
            ok: true,
            log: &mouse.log,
        };
        let report = stop(&mut procs, &mouse);
        assert_eq!(*mouse.log.borrow(), vec!["end", "release left"]);
        assert_eq!(
            report,
            StopReport {
                ended: true,
                released: vec!["left"],
                warning: None
            }
        );
    }

    #[test]
    fn nothing_held_nothing_posted() {
        let mouse = mouse(vec![]);
        let mut procs = FakeProcs {
            ok: true,
            log: &mouse.log,
        };
        let report = stop(&mut procs, &mouse);
        assert!(report.released.is_empty());
        assert_eq!(*mouse.log.borrow(), vec!["end"]);
    }

    #[test]
    fn says_so_when_a_button_stays_held_or_a_process_stays() {
        let mut stuck = mouse(vec![Button::Left, Button::Right]);
        stuck.sticky = true;
        let mut procs = FakeProcs {
            ok: true,
            log: &stuck.log,
        };
        let report = stop(&mut procs, &stuck);
        assert!(report.warning.unwrap().contains("left and right"));
        let free = mouse(vec![]);
        let mut procs = FakeProcs {
            ok: false,
            log: &free.log,
        };
        assert!(stop(&mut procs, &free)
            .warning
            .unwrap()
            .contains("did not end"));
        let mut blind = mouse(vec![]);
        blind.readable = false;
        let mut procs = FakeProcs {
            ok: true,
            log: &blind.log,
        };
        assert!(stop(&mut procs, &blind)
            .warning
            .unwrap()
            .contains("could not be read"));
    }
}
