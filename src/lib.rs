//! # Addy
//! A library for ergonomically handling kernel interrupts.
//!
//! ## Quick Start
//! ```no_run
//! use addy::SIGWINCH;	
//! use std::io::{Read, stdin};
//! fn main() -> Result<(), addy::Error> {
//! 	/* SIGWINCH is a POSIX interrupt signal for window resized */
//! 	addy::mediate(SIGWINCH)
//! 			.register("print", |_signal| { println!("Screen Resized!"); })?
//! 			.enable()?;
//!
//! 	/* Block so the program doesn't exit immediately
//! 	 * Try resizing your terminal window :)
//! 	*/
//! 	let mut buffer = [0; 1];
//!    	loop {
//!        stdin().read(&mut buffer);
//!   	}
//! 	Ok(())
//! }
//! ```
//!
//! # Things To Know
//! I love you and I wish the best for you. No matter what you choose to do, I hope you decide it is worth you time to do it well.
//!
//! ## Addy is Thread Safe!
//! You can call it from anywhere, at anytime! You can store a SignalHandle (returned from addy::mediate(signal)) in a variable and pass it around.
//! ```no_run
//! use addy::{SIGWINCH, SIGINT};
//! use std::io::{Read, stdin};
//! static QUOTE: &'static str = "Look at you, hacker: a pathetic creature of meat \
//! 							  and bone, panting and sweating as you run through \
//! 							  my corridors. How can you challenge a perfect, \
//! 							  immortal machine?";
//!
//! fn main() -> Result<(), addy::Error> {
//! 	/* When the window resizes */
//!     addy::mediate(SIGWINCH)
//!     		.register("hello", |_signal| { println!("Hello, World!"); })?
//!     		.register("girls", |_signal| { println!("Hello, Girls!"); })?
//!     		.enable()?;
//!
//!     /* SIGINT is sent when the user presses Ctrl + C. The default behavior is
//!      * to interrupt the program's execution.
//!     */
//!     let mut ctrl_c = addy::mediate(SIGINT);
//!     ctrl_c.register("no_interruptions", |_signal| { println!("{}", QUOTE); })?.enable()?;
//!
//!     /* Let the user use Ctrl + C to kill the program after 10 seconds */
//!     std::thread::spawn(move || -> Result<(), addy::Error> {
//!         std::thread::sleep(std::time::Duration::from_secs(10));
//!         ctrl_c.default()?;
//!         Ok(())
//!     });
//!
//!     /* Stop saying "Hello, World!" on resize after 5 seconds */
//!     std::thread::spawn(move || -> Result<(), addy::Error> {
//!         std::thread::sleep(std::time::Duration::from_secs(5));
//!         addy::mediate(SIGWINCH).remove("hello")?;
//!         Ok(())
//!     });
//!
//!     /* Capture the input so we don't exit the program immediately */
//!     let mut buffer = [0; 1];
//!     loop {
//!         stdin().read(&mut buffer);
//!     }
//!
//!     Ok(())
//! }
//! ```
//! ## Errors
//! If the MPSC channel closes, of the Event Loop thread closes, there is no way to recover and any future Addy calls will return an addy::Error.

#![deny(
    missing_docs,
    missing_debug_implementations,
    missing_copy_implementations,
    trivial_casts,
    trivial_numeric_casts,
    unstable_features,
    unused_import_braces,
    unused_qualifications
)]

/* Standard Library */
use std::convert::TryFrom;
use std::sync::{
    mpsc::{self, Sender},
    Mutex, Once,
};
use std::thread;

/* Std Lib Adjacent Crates */
use lazy_static::lazy_static;
use libc;

/* Thrid Party Crates */
use fnv::FnvHashMap; // Faster for the interger keys we're using

/**********
 * ERRORS *
 **********/
/* Use our own error instead of passing the SendError<Action> so we don't have
 * expose the Action enum publicly.
*/
#[derive(Debug, Clone, Copy)]
/// Addy Error type - realistically you will never see it. As it only occurs
/// when the MPSC channel fails. MPSC channels only fail if the receiver is
/// dropped which can only happen if the event loop thread panics somehow.
///
/// If it does fail, there is no way to recover, future Addy calls will fail.
pub enum Error {
    /// Returned when a function call on a SignalHandler fails.
    CallFailed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::CallFailed => write!(
                f,
                "Addy function call failed to send. The MPSC and/or event loop thread has closed."
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/***********
 * ACTIONS *
 ***********/

/* Used by the MPSC channel to instruct what the Event Loop should do when
 * it wakes up.
 *
 * ==========================================================================
 *
 * CBPointer is a how Addy represents "pointers" to the callbacks the caller
 * passes in with .register()
 *
 * CBP wraps CBPointer so Debug can be implemented for it
*/
type CBPointer = Box<dyn Fn(Signal) -> () + Send>;
struct CBP(CBPointer);
impl std::fmt::Debug for CBP {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CBPointer")
    }
}

/* This enum is what is message passed to the Event Loop to tell it what
 * action to take.
*/
#[derive(Debug)]
enum Action {
    // Used by fn c_handler(...) to tell the Event Loop an interrupt occured
    Call(Signal),
    // Used by SignalHandle to add a named callback for the associated interrupt
    Register(Signal, String, CBP),
    // Used by SignalHandle to remove a named callback from the associated interrupt
    Remove(Signal, String),
    /* Used by SignalHandle to clear all the callbacks from the associated
     * intterupt. This effectively ignores the interrupt, but the signal is
     * still handled by this library and the signal handler. If you're clearing
     * to stop callbacks, but don't plan on adding anymore use Release instead.
    	*/
    Clear(Signal),
    // Used by SignalHandle to prevent the default signal behavior from occurring
    Ignore(Signal),
    /* Used by SignalHandle to restore the interrupt handler to the default
     * behavior (like terminating your program). Some interrupt's default
     * behavior is to be ignored.
    	*/
    Default(Signal),
    /* Used by SignalHandle to stop handling the associated interrupt. Resets
     * the interrupts behavior to default and clears all callbacks.
    	*/
    Release(Signal),
    /* Used by SignalHandle to tell Addy to resume handling this intterupt.
     * e.g. if you registered 3 callbacks, then set the interrupt handler to
     * .ignore() or .default(), then later called .resume() the 3 callbacks
     * would be called again when the interrupt occurs.
     *
     * This is also aliased by SignalHandle .enable() to start capturing the
     * interrupt.
    	*/
    Resume(Signal),
}

/***********
 * SIGNALS *
 ***********/

/* Most of this section is ripped & modified from the nix* crate so I didn't
 * have to retype every signal and look up every architecture difference.
 *
 * Crate: https://crates.io/crates/nix
 * Source: https://github.com/nix-rust/nix/blob/7a5248c70a4ad0ef1ff1b385a7674b38403386df/src/sys/signal.rs#L20
 * License: (MIT) - https://github.com/nix-rust/nix/blob/master/LICENSE
 *
 * Representing the Signals as i32 (libc::c_int) so we can use Rust's features
 * around enums.
*/

/* Required to we can use them in our callback HashMaps */
/// Enum representing the different interrupt signals
///
/// # Signals Supported
/// Not all signals are supported on all platforms/architectures. Which signals
/// does your platform support? Run: `kill -l` to find out!
///
/// * SIGHUP
/// * SIGINT
/// * SIGQUIT
/// * SIGILL
/// * SIGTRAP
/// * SIGABRT
/// * SIGBUS
/// * SIGFPE
/// * SIGKILL
/// * SIGUSR1
/// * SIGSEGV
/// * SIGUSR2
/// * SIGPIPE
/// * SIGALRM
/// * SIGTERM
/// * SIGSTKF
/// * SIGCHLD
/// * SIGCONT
/// * SIGSTOP
/// * SIGTSTP
/// * SIGTTIN
/// * SIGTTOU
/// * SIGURG
/// * SIGXCPU
/// * SIGXFSZ
/// * SIGVTAL
/// * SIGPROF
/// * SIGWINC
/// * SIGIO  
/// * SIGPWR
/// * SIGSYS
/// * SIGEMT
/// * SIGINFO
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Signal {
    /// Hangup detected on controlling terminal or death of controlling process
    SIGHUP = libc::SIGHUP,
    /// Interrupt from keyboard
    SIGINT = libc::SIGINT,
    /// Quit from keyboard
    SIGQUIT = libc::SIGQUIT,
    /// Illegal Instruction
    SIGILL = libc::SIGILL,
    /// Trace/breakpoint trap
    SIGTRAP = libc::SIGTRAP,
    /// Abort signal from abort(3)
    SIGABRT = libc::SIGABRT,
    /// Bus error (bad memory access)
    SIGBUS = libc::SIGBUS,
    /// Floating-point exception
    SIGFPE = libc::SIGFPE,
    /// Kill signal
    SIGKILL = libc::SIGKILL,
    /// User-defined signal 1
    SIGUSR1 = libc::SIGUSR1,
    /// Invalid memory reference
    SIGSEGV = libc::SIGSEGV,
    /// User-defined signal 2
    SIGUSR2 = libc::SIGUSR2,
    /// Broken pipe: write to pipe with no readers
    SIGPIPE = libc::SIGPIPE,
    /// Timer signal from alarm(2)
    SIGALRM = libc::SIGALRM,
    /// Termination signal
    SIGTERM = libc::SIGTERM,
    /// Stack fault on coprocessor.
    #[cfg(all(
        any(target_os = "android", target_os = "emscripten", target_os = "linux"),
        not(any(target_arch = "mips", target_arch = "mips64", target_arch = "sparc64"))
    ))]
    SIGSTKFLT = libc::SIGSTKFLT,
    /// Child stopped or terminated
    SIGCHLD = libc::SIGCHLD,
    /// Continue if stopped
    SIGCONT = libc::SIGCONT,
    /// Stop process
    SIGSTOP = libc::SIGSTOP,
    /// Stop typed at terminal
    SIGTSTP = libc::SIGTSTP,
    /// Terminal input for background process
    SIGTTIN = libc::SIGTTIN,
    /// Terminal output for background process
    SIGTTOU = libc::SIGTTOU,
    /// Urgent condition on socket (4.2BSD)
    SIGURG = libc::SIGURG,
    /// CPU time limit exceeded (4.2BSD)
    SIGXCPU = libc::SIGXCPU,
    /// File size limit exceeded (4.2BSD)
    SIGXFSZ = libc::SIGXFSZ,
    /// Virtual alarm clock (4.2BSD)
    SIGVTALRM = libc::SIGVTALRM,
    /// Profiling timer expired
    SIGPROF = libc::SIGPROF,
    /// Window resize signal (4.3BSD, Sun)
    SIGWINCH = libc::SIGWINCH,
    /// I/O now possible (4.2BSD)
    SIGIO = libc::SIGIO,
    /// Power failure (System V)
    #[cfg(any(target_os = "android", target_os = "emscripten", target_os = "linux"))]
    SIGPWR = libc::SIGPWR,
    /// Bad system call (SVr4)
    SIGSYS = libc::SIGSYS,
    /// Emulator trap
    #[cfg(not(any(target_os = "android", target_os = "emscripten", target_os = "linux")))]
    SIGEMT = libc::SIGEMT,
    /// A synonym for SIGPWR
    #[cfg(not(any(target_os = "android", target_os = "emscripten", target_os = "linux")))]
    SIGINFO = libc::SIGINFO,
}

/* Re-export all the Signals without the prefix.
 * Mad that I didn't know you could do this, I had a Signal enum and switched to
 * constants for aesthetic reasons.
*/
pub use self::Signal::*;

impl Signal {
    /* Used so Signal can implement Display */

    /// Returns name of signal.
    ///
    /// This function is equivalent to `<Signal as AsRef<str>>::as_ref()`,
    /// with difference that returned string is `'static`
    /// and not bound to `self`'s lifetime.
    ///
    /// # Example
    /// ```no_run
    /// use addy::SIGINT;
    ///
    /// fn main() {
    /// 	println!("My favorite interrupt is: {}", SIGINT);
    /// }
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            SIGHUP => "SIGHUP",
            SIGINT => "SIGINT",
            SIGQUIT => "SIGQUIT",
            SIGILL => "SIGILL",
            SIGTRAP => "SIGTRAP",
            SIGABRT => "SIGABRT",
            SIGBUS => "SIGBUS",
            SIGFPE => "SIGFPE",
            SIGKILL => "SIGKILL",
            SIGUSR1 => "SIGUSR1",
            SIGSEGV => "SIGSEGV",
            SIGUSR2 => "SIGUSR2",
            SIGPIPE => "SIGPIPE",
            SIGALRM => "SIGALRM",
            SIGTERM => "SIGTERM",
            #[cfg(all(
                any(target_os = "android", target_os = "emscripten", target_os = "linux"),
                not(any(target_arch = "mips", target_arch = "mips64", target_arch = "sparc64"))
            ))]
            SIGSTKFLT => "SIGSTKFLT",
            SIGCHLD => "SIGCHLD",
            SIGCONT => "SIGCONT",
            SIGSTOP => "SIGSTOP",
            SIGTSTP => "SIGTSTP",
            SIGTTIN => "SIGTTIN",
            SIGTTOU => "SIGTTOU",
            SIGURG => "SIGURG",
            SIGXCPU => "SIGXCPU",
            SIGXFSZ => "SIGXFSZ",
            SIGVTALRM => "SIGVTALRM",
            SIGPROF => "SIGPROF",
            SIGWINCH => "SIGWINCH",
            SIGIO => "SIGIO",
            #[cfg(any(target_os = "android", target_os = "emscripten", target_os = "linux"))]
            SIGPWR => "SIGPWR",
            SIGSYS => "SIGSYS",
            #[cfg(not(any(target_os = "android", target_os = "emscripten", target_os = "linux")))]
            SIGEMT => "SIGEMT",
            #[cfg(not(any(target_os = "android", target_os = "emscripten", target_os = "linux")))]
            SIGINFO => "SIGINFO",
        }
    }
}

impl AsRef<str> for Signal {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/* We can now print the Signal */
impl std::fmt::Display for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}

/* Array of Signal, platform dependent */
#[cfg(all(
    any(target_os = "linux", target_os = "android", target_os = "emscripten"),
    not(any(target_arch = "mips", target_arch = "mips64", target_arch = "sparc64"))
))]
const SIGNALS: [Signal; 31] = [
    SIGHUP, SIGINT, SIGQUIT, SIGILL, SIGTRAP, SIGABRT, SIGBUS, SIGFPE, SIGKILL, SIGUSR1, SIGSEGV,
    SIGUSR2, SIGPIPE, SIGALRM, SIGTERM, SIGSTKFLT, SIGCHLD, SIGCONT, SIGSTOP, SIGTSTP, SIGTTIN,
    SIGTTOU, SIGURG, SIGXCPU, SIGXFSZ, SIGVTALRM, SIGPROF, SIGWINCH, SIGIO, SIGPWR, SIGSYS,
];

#[cfg(all(
    any(target_os = "linux", target_os = "android", target_os = "emscripten"),
    any(target_arch = "mips", target_arch = "mips64", target_arch = "sparc64")
))]
const SIGNALS: [Signal; 30] = [
    SIGHUP, SIGINT, SIGQUIT, SIGILL, SIGTRAP, SIGABRT, SIGBUS, SIGFPE, SIGKILL, SIGUSR1, SIGSEGV,
    SIGUSR2, SIGPIPE, SIGALRM, SIGTERM, SIGCHLD, SIGCONT, SIGSTOP, SIGTSTP, SIGTTIN, SIGTTOU,
    SIGURG, SIGXCPU, SIGXFSZ, SIGVTALRM, SIGPROF, SIGWINCH, SIGIO, SIGPWR, SIGSYS,
];

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
const SIGNALS: [Signal; 31] = [
    SIGHUP, SIGINT, SIGQUIT, SIGILL, SIGTRAP, SIGABRT, SIGBUS, SIGFPE, SIGKILL, SIGUSR1, SIGSEGV,
    SIGUSR2, SIGPIPE, SIGALRM, SIGTERM, SIGCHLD, SIGCONT, SIGSTOP, SIGTSTP, SIGTTIN, SIGTTOU,
    SIGURG, SIGXCPU, SIGXFSZ, SIGVTALRM, SIGPROF, SIGWINCH, SIGIO, SIGSYS, SIGEMT, SIGINFO,
];

/* Count of the above signal constants + 1. Used to create HashMaps.with_capacity()
 * and with from libc::c_int for array bounds checking.
*/
const NUM_SIGNALS: libc::c_int = 32;

/*******************
 * SIGNAL ITERATOR *
 *******************/

/// Useful if you want to set every signal to "Ignore" or "Default."
///
/// # Example
/// ```no_run
/// use addy::Signal;
///
/// fn main() -> Result<(), addy::Error> {
/// 	/* Have each intterupt print itself */
///     for signal in Signal::iterator() {
/// 		addy::mediate(signal).register("reflexive", |signal| {
///				println!("Signal: {}", signal);
///			})?;
///		}
///		Ok(())
/// }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SignalIterator {
    next: usize,
}

impl Iterator for SignalIterator {
    type Item = Signal;

    fn next(&mut self) -> Option<Signal> {
        if self.next < SIGNALS.len() {
            let next_signal = SIGNALS[self.next];
            self.next += 1;
            Some(next_signal)
        } else {
            None
        }
    }
}

impl Signal {
    /// Returns an iterator over the different Signals
    pub fn iterator() -> SignalIterator {
        SignalIterator { next: 0 }
    }
}

/******************
 * C FFI CALLBACK *
 ******************/

/* This is the callback passed to the C FF sigaction(...) - it is called with
 * three arguments. We only care about what signal was called so we free() the
 * other two, grab a copy of Sender and message pass what signal was called to
 * the Event Loop.
*/
type CVoid = *mut libc::c_void;
fn c_handler(signal: Signal, info: *mut libc::siginfo_t, ucontext: CVoid) {
    /* Free the pointers to the info_t and ucontenxt_t structs returned to us */
    unsafe {
        if info != std::ptr::null_mut() {
            libc::free(info as CVoid);
        }
        if ucontext != std::ptr::null_mut() {
            libc::free(ucontext);
        }
    }

    /* We're the only function that interacts with this global static copy of
     * a sender to the Event Loop. We only read from this location, only one
     * interrupt can be active at a time so this is SAFE.
    	*/
    let sender;
    unsafe {
        sender = SENDER.as_ref().unwrap().clone();
    }

    /* Drop the error since we can't return one from across the kernel
     * boundary.
    	*/
    let _ = sender.send(Action::Call(signal));
}

/*****************
 * SIGNAL HANDLE *
 *****************/

/// This is the struct returned from an addy::mediate(Signal) call. It allows
/// the caller to add, remove, and clear callbacks to the provided interrupt
/// handler. Adding closures prevents the default bevaior (if any).
///
/// You can also set the interrupt to the default behaviour, or to be ignored by
/// the process. If you set a signal to be ignored or back to the defaults you
/// can call .resume() to to have it handle your callbacks again.
///
/// Dropping it does _not_ stop the signal handler. You must call .release() to
/// have Addy stop handling this interrupt and free the associated resources.
/// Conversely, you don't have to keep a handle to this around once you've set
/// it up.
///
/// If you register callbacks for an interrupt, you must call .enable() to have
/// them run. If you call .release() on a SignalHandler you must call .enable()
/// again (after re-registering new callbacks).
///
/// # Example
/// ```no_run
/// use addy::{Signal, SIGWINCH};
///
/// fn my_func(signal: Signal) {
/// 	/* Does a thing */
/// }
///
/// fn main() -> Result<(), addy::Error> {
/// 	addy::mediate(SIGWINCH)
///				.register("print", |_signal| { println!("Screen Resized!"); })?
///				.register("my_func", my_func)?
///				.enable()?;
///
///		//-- Later --//
///
///		// Ignore the incoming signal
/// 	addy::mediate(SIGWINCH).ignore()?;
///
///		//-- Later Still --//
///
///		// Swap out one of the callbacks and re-enable capturing the interrupt
/// 	addy::mediate(SIGWINCH)
///				.remove("print")?
/// 			.register("print", |_signal| { println!("New Output!"); })?
///				.enable()?;
///
///		Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct SignalHandle {
    signal: Signal,
    sender: Sender<Action>,
}

/* Convenient Type Alias */
type SignalResult<'a> = Result<&'a mut SignalHandle, Error>;

impl SignalHandle {
    /// Registers a callback with the interrupt handler for the associated
    /// Signal. If you call register with the same name it will replace the
    /// previous callback.
    ///
    /// # Example
    /// ```no_run
    /// use addy::{Signal, SIGWINCH};
    ///
    /// fn my_func(signal: Signal) {
    /// 	/* Does a thing */
    /// }
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGWINCH)
    ///				.register("print", |_signal| { println!("Screen Resized!"); })?
    ///				.register("my_func", my_func)?
    ///				.enable()?;
    ///
    ///		Ok(())
    /// }
    /// ```
    pub fn register<'a, A, F>(&'a mut self, name: A, cb: F) -> SignalResult
    where
        A: AsRef<str>,
        F: Fn(Signal) -> () + Send + 'static,
    {
        /* Box the Callback */
        let cb = CBP(Box::new(cb));
        let name = String::from(name.as_ref());
        self.sender
            .send(Action::Register(self.signal, name, cb))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }

    /// Removes a named callback from the associated Signal. If no callback with
    /// that name exists, it does nothing.
    ///
    /// # Example
    /// ```no_run
    /// use addy::{Signal, SIGWINCH};
    ///
    /// fn my_func(signal: Signal) {
    /// 	/* Does a thing */
    /// }
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGWINCH)
    ///				.register("print", |_signal| { println!("Screen Resized!"); })?
    ///				.register("my_func", my_func)?
    ///				.enable()?;
    ///
    ///		//-- Later --//
    ///
    ///		// Stop calling "print" when the process receives a SIGWINCH signal
    /// 	addy::mediate(SIGWINCH).remove("print")?;
    ///
    ///		Ok(())
    /// }
    /// ```
    pub fn remove<'a, A>(&'a mut self, name: A) -> SignalResult
    where
        A: AsRef<str>,
    {
        let name = String::from(name.as_ref());
        self.sender
            .send(Action::Remove(self.signal, name))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }

    /// Removes a all callbacks from the associated Signal. Functionally similar
    /// to calling .ignore() except you don't need to call .enable() if you add
    /// new callbacks later.
    ///
    /// # Example
    /// ```no_run
    /// use addy::{Signal, SIGWINCH};
    ///
    /// fn my_func(signal: Signal) {
    /// 	/* Does a thing */
    /// }
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGWINCH)
    ///				.register("print", |_signal| { println!("Screen Resized!"); })?
    ///				.register("my_func", my_func)?
    ///				.enable()?;
    ///
    ///		//-- Later --//
    ///
    ///		// Capture the signal, but stop calling anything
    /// 	addy::mediate(SIGWINCH)
    /// 			.clear()?
    ///				.register("solo_callback", |_signal| { println!("ALONE!"); })?;
    ///
    ///		Ok(())
    /// }
    /// ```
    pub fn clear<'a>(&'a mut self) -> SignalResult {
        self.sender
            .send(Action::Clear(self.signal))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }

    /// Removes a all callbacks from the associated Signal and resets the
    /// interrupt handler to the default behavior. Funcationally the same as
    /// calling .clear() and .default().
    ///
    /// You will need to call .enable() again after re-registering callbacks.
    ///
    /// # Example
    /// ```no_run
    /// use addy::SIGWINCH;
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGWINCH)
    ///				.register("print", |_signal| { println!("Screen Resized!"); })?
    ///				.enable()?;
    ///
    ///		//-- Later --//
    ///
    ///		// Stop capturing the signal
    /// 	addy::mediate(SIGWINCH).release()?;
    ///  
    /// 	//-- Later Still --//
    ///
    /// 	// Start catpuring again
    ///		addy::mediate(SIGWINCH)
    ///				.register("new", |_signal| { println!("New callback!"); })?
    ///				.enable()?;
    ///
    ///		Ok(())
    /// }
    /// ```
    pub fn release<'a>(&'a mut self) -> SignalResult {
        self.sender
            .send(Action::Release(self.signal))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }

    /// Tells the process to ignore this interrupt. Keeps all your callbacks.
    /// Calling .resume() will re-enable them.
    ///
    /// # Example
    /// ```no_run
    /// use addy::SIGWINCH;
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGWINCH)
    ///				.register("print", |_signal| { println!("Screen Resized!"); })?
    ///				.enable()?;
    ///
    ///		//-- Later --//
    ///
    ///		// Ignore the signal
    /// 	addy::mediate(SIGWINCH).ignore()?;
    ///
    /// 	//-- Later Still --//
    ///
    /// 	// Start catpuring again
    ///		addy::mediate(SIGWINCH).resume()?;
    ///
    ///		Ok(())
    /// }
    /// ```
    pub fn ignore<'a>(&'a mut self) -> SignalResult {
        self.sender
            .send(Action::Ignore(self.signal))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }

    /// Restore the interrupt handler to the system default. Not all interrupts
    /// have a default, and some interrupts default is to be ignored. Keeps all
    /// your callbacks. Calling .resume() will re-enable them.
    ///
    /// # Example
    /// ```no_run
    /// use addy::SIGINT;
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGINT)
    ///				.register("print", |_signal| { println!("Interrupted!"); })?
    ///				.enable()?;
    ///
    ///		//-- Later --//
    ///
    ///		// Set the signal to its default
    /// 	addy::mediate(SIGINT).default()?;
    ///
    /// 	//-- Later Still --//
    ///
    /// 	// Start catpuring again
    ///		addy::mediate(SIGINT).resume()?;
    ///
    ///		Ok(())
    /// }
    /// ```
    pub fn default<'a>(&'a mut self) -> SignalResult {
        self.sender
            .send(Action::Default(self.signal))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }

    /// Resumes capturing the interrupt and calling any associated callbacks.
    /// Most often used after a call to .ignore() and .default().
    ///
    /// Alias of .enable()
    ///
    /// # Example
    /// ```no_run
    /// use addy::SIGINT;
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGINT)
    ///				.register("print", |_signal| { println!("Interrupted!"); })?
    ///				.enable()?;
    ///
    ///		//-- Later --//
    ///
    ///		// Set the signal to its default
    /// 	addy::mediate(SIGINT).default()?;
    ///
    /// 	//-- Later Still --//
    ///
    ///		// Start catpuring and printing "Interrupted!" again
    ///		addy::mediate(SIGINT).resume()?;
    ///
    ///		Ok(())
    /// }
    /// ```
    pub fn resume<'a>(&'a mut self) -> SignalResult {
        self.sender
            .send(Action::Resume(self.signal))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }

    /// Begins capturing the interrupt and calling any associated callbacks.
    /// Most often used after a calls .register()
    ///
    /// Alias of .resume()
    ///
    /// # Example
    /// ```no_run
    /// use addy::SIGINT;
    ///
    /// fn main() -> Result<(), addy::Error> {
    /// 	addy::mediate(SIGINT)
    ///				.register("print", |_signal| { println!("Interrupted!"); })?
    ///				.enable()?;
    ///		Ok(())
    /// }
    /// ```
    pub fn enable<'a>(&'a mut self) -> SignalResult {
        self.sender
            .send(Action::Resume(self.signal))
            .map_err(|_| Error::CallFailed)?;
        Ok(self)
    }
}

/**************************************
 * SETUP EVENT LOOP & MPSC CHANNEL *
 **************************************/
/* This is the thread that the different interrupt handlers send messages to.
 * when they occur. They message what they want done and this thread executes it.
*/

/* This closure can only be called at most ONCE - allows us to ensure the Event
 * Loop is set up a maximum of one time. This also means that if the MPSC
 * channel ever fails we can't recover from it.
*/
static SETUP: Once = Once::new();

/* FUTURE: Consider removing this to remove the dependency on lazy_static!()
 * This gets set up ONCE and then only read from. The downside is more
 * unsafe {} blocks :<
 *
 * Currently SENDER is only accessed in one place, that can only be run one at
 * a time (i.e. in an interrupt) and copies of SAFE_SENDER can be made from
 * any thread at any time. Still... it's read only...
*/
lazy_static! {
    /* MPSC channel used by interrupts to communicate to the Event Loop. This
     * stores a global copy of a Sender that can be cloned and given to the
     * various interrupt handlers as they are created.
    */
    static ref SAFE_SENDER: Mutex<Option<Sender<Action>>> = {
        Mutex::new(None)
    };
}

/* C FFI MESSAGE PASSER
 *
 * Copy of a sender to the Event Loop. It is only setup ONCE on the first
 * addy::mediate() call. The setup always occurs before it is READ from as it is
 * set before any handler is registered (the only place that attempts to read
 * from this static global).
*/
static mut SENDER: Option<Sender<Action>> = None;

/* This is the initial Addy setup. It sets up the Event Loop and the MPCS
 * channel. Setup occurs on the first call of addy::mediate(Signal).
*/

type NameToCallback = FnvHashMap<String, CBP>;
type SignalToCallbacks<T> = FnvHashMap<Signal, T>;
fn setup() {
    /* Only setup the Event Loop once */
    SETUP.call_once(|| {
        // we may need to block on "completed" to make sure this is completed
        // Setup an async MPSC channel - the receiver will be the Event Loop
        let (sender, receiver) = mpsc::channel::<Action>();

        /* Save a copy of a sender to a global variable so it can be
         * clone()'d and handed off to future singal handlers structs.
        	*/
        {
            let mut guard = SAFE_SENDER.lock().unwrap();
            guard.replace(sender.clone());
        }

        /* Save a copy of the sender in an global static mut Option
         *
         * This is SAFE because this is only called ONCE and the only other
         * place this is accessed is in fn  c_handler() which cannot be called
         * before this setup is run. In addition, only one interrupt handler can
         * be running at a time, which is why this convolution is necessary.
        	*/
        unsafe {
            SENDER.replace(sender.clone());
        }

        /**************
         * EVENT LOOP *
         **************/

        /* Spawn the Event Loop thread, pass the receiver to it. */
        thread::spawn(move || {
            /* Create a map from Signal -> Map<Name, Closure> */
            let nsig = usize::try_from(NUM_SIGNALS).unwrap(); // i32(32) - constant we control :)
            let mut handlers = SignalToCallbacks::<NameToCallback>::with_capacity_and_hasher(
                nsig,
                Default::default(),
            );

            /* Stores if we need to re-establish fn c_handler() as the interrupt
             * handler. e.g. if the user called .ignore() and then .resume()
            	*/
            let mut active: [bool; NUM_SIGNALS as usize] = [false; 32];

            /*************
             * CONSTANTS *
             *************/
            /* SigAction Structs to represent the SIG_DFL, SIG_IGN and custom
             * handler. These are passed to libc::sigaction(...) to tell it what
             * to do when a signal is called. They tell it to perform the
             * default action, ignore the signal or run the list of user
             * registered callbacks respectively.
            	*/
            const SA_DEFAULT: libc::sigaction = libc::sigaction {
                sa_sigaction: libc::SIG_DFL,
                sa_mask: 0,
                sa_flags: libc::SA_SIGINFO,
            };

            const SA_IGNORE: libc::sigaction = libc::sigaction {
                sa_sigaction: libc::SIG_IGN,
                sa_mask: 0,
                sa_flags: libc::SA_SIGINFO,
            };

            /* Q: Why isn't this a constant?
             * A: Converting function pointers to integers in a constant is
             * unstable. (Yes I tried the various workarounds)
             *
             * Link: https://github.com/rust-lang/rust/issues/51910
            	*/
            #[allow(non_snake_case)]
            let SA_CALLBACK: libc::sigaction = libc::sigaction {
                sa_sigaction: c_handler as libc::sighandler_t,
                sa_mask: 0,
                sa_flags: libc::SA_SIGINFO,
            };

            /***************************************
             * HELPER FUNCTIONS TO KEEP THINGS DRY *
             ***************************************/
            /* Tells the process to ignore the interrupt */
            fn ignore(signal: Signal) {
                /* SA_IGN is a static sigaction struct with a
                 * special ignore handler value.
                	*/
                unsafe {
                    libc::sigaction(signal as libc::c_int, &SA_IGNORE, std::ptr::null_mut());
                }
            }
            /* Sets the interrupt handler to the default value */
            fn default(signal: Signal) {
                /* SA_DFL is a static sigaction struct with a
                 * special reset to default handler value.
                	*/
                unsafe {
                    libc::sigaction(signal as libc::c_int, &SA_DEFAULT, std::ptr::null_mut());
                }
            }
            /* Trys to convert a Signal to a USize to index into active[] */
            fn index(signal: Signal) -> usize {
                usize::try_from(signal as libc::c_int).unwrap()
            }
            /* Resets all signals to their default behaviour. Does not clear out
             * registered handlers.
            	*/
            fn set_all_to_default() {
                for signal in Signal::iterator() {
                    default(signal);
                }
            }

            /*********
             * PANIC *
             *********/

            /* If this thread panics for any reason, set all signals to the
             * default behavior.
            	*/
            let _ = std::panic::catch_unwind(|| {
                set_all_to_default();
            });

            /**************
             * EVENT LOOP *
             **************/

            /* Returns None when the channel is closed. */
            let mut messages = receiver.iter();
            while let Some(action) = messages.next() {
                match action {
                    Action::Call(signal) => {
                        /* Get the map of callbacks for this signal */
                        if let Some(callbacks) = handlers.get(&signal) {
                            /* Call each callback */
                            let callbacks = callbacks.iter();
                            for (_, cb) in callbacks {
                                cb.0(signal);
                            }
                        }
                    }
                    Action::Register(signal, name, cb) => {
                        /* Get the map of callbacks for this signal */
                        let callbacks = handlers.entry(signal).or_default();
                        callbacks.insert(name, cb);
                    }
                    Action::Remove(signal, name) => {
                        /* Get the map of callbacks for this signal */
                        if let Some(callbacks) = handlers.get_mut(&signal) {
                            callbacks.remove(&name);
                        }
                    }
                    Action::Clear(signal) => {
                        handlers.remove(&signal);
                    }
                    Action::Ignore(signal) => {
                        ignore(signal);
                        active[index(signal)] = false;
                    }
                    Action::Default(signal) => {
                        default(signal);
                        active[index(signal)] = false;
                    }
                    Action::Release(signal) => {
                        /* Clear the callback map */
                        handlers.remove(&signal);

                        /* Set the handler back to the defaults */
                        default(signal);
                        active[index(signal)] = false;
                    }
                    Action::Resume(signal) => {
                        /* Check to see if it's already setup up */
                        if !active[index(signal)] {
                            unsafe {
                                /* SA_CALLBACK is a static sigaction struct that
                                 * points to c_handler(...)
                                	*/
                                libc::sigaction(
                                    signal as libc::c_int,
                                    &SA_CALLBACK,
                                    std::ptr::null_mut(),
                                );
                            }
                            active[index(signal)] = true;
                        }
                    }
                }
            } // </Event Loop>

            /* If the thread closes - set all the singals back to their default
             * behavior and remove all callbacks.
            */
            set_all_to_default();
        }); // </Thread>
    }); // </Once>

    /* There's a chance that the ONCE call actually initialized something else
     * and that we're not ready so we spin until we are. Probably not necessary.
     *
     * Apparently it's only available on nightly, but is merged in and will be 
     * stable shortly. See link below for detail:
     * 
     * Link: https://github.com/rust-lang/rust/issues/54890
    */
    #[cfg(feature = "nightly")]
    while !SETUP.is_completed() { /*-- ᓚᘏᗢ --*/ }
}

/***********
 * MEDIATE *
 ***********/

/* If this is the FIRST time new has been called, for _any_ Signal it
 * will set up the Event Loop thread and MPCS handlers as well.
*/
/// Use this to get a SignalHandle representing a interrupt specified by Signal.
///
/// # Example
/// ```no_run
/// use addy::SIGWINCH;
///	use std::io::{Read, stdin};
///
/// fn main() -> Result<(), addy::Error> {
/// 	/* SIGWINCH is a POSIX interrupt signal */
/// 	addy::mediate(SIGWINCH)
///				.register("resized", |_signal| { println!("Screen Resized!"); })?
///				.enable()?;
///
///		/* Block so the program doesn't exit immediately
/// 	 * Try resizing your terminal window :)
/// 	*/
///		let mut buffer = [0; 1];
///    	loop {
///        stdin().read(&mut buffer);
///   	}
///
/// 	Ok(())
/// }
/// ```
pub fn mediate<S: Into<Signal>>(signal: S) -> SignalHandle {
    let signal = signal.into();

    /* Performs the initial setup for all handlers - only called ONCE */
    setup();

    /* Create a clone() of the Sender so we can pass messages to the Event
     * Loop from the returned struct.
    	*/
    let sender;
    {
        let guard = SAFE_SENDER.lock().unwrap();
        sender = guard.as_ref().unwrap().clone();
    }

    SignalHandle { signal, sender }
}

/* Alternative, arcane, profane function aliases for addy::mediate(...) */
#[doc(hidden)]
pub fn medicate(signal: Signal) {
    mediate(signal);
}
#[doc(hidden)]
pub fn intercept(signal: Signal) {
    mediate(signal);
}
