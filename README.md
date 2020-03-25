# Addy 
A library for ergonomically handling kernel interrupts.

## Quick Start
```rust
use addy::SIGWINCH;
use std::io::{Read, stdin};
fn main() -> Result<(), addy::Error> {
	/* SIGWINCH is a POSIX interrupt signal for window resized */
	addy::mediate(SIGWINCH)
			.register("print", |_signal| { println!("Screen Resized!"); })?
			.enable()?;

	/* Block so the program doesn't exit immediately 
	 * Try resizing your terminal window :)
	*/
	let mut buffer = [0; 1];
   	loop {
       stdin().read(&mut buffer);
  	}
	Ok(())
}
```

# Functions

## Mediate
This gives you a SignalHandle representing the interrupt handler you want to interact with. SignalHandles are threadsafe! Call/create/move them anywhere from anywhere!
```rust
use addy::SIGWINCH;
use std::io::{Read, stdin};

fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGWINCH)
			.register("resized", |_signal| { println!("Screen Resized!"); })?
			.enable()?;

	/* Block so the program doesn't exit immediately 
	 * Try resizing your terminal window :)
	*/
	let mut buffer = [0; 1];
	loop {
    	stdin().read(&mut buffer);
	}

	Ok(())
}
 ```

## Register
Registers a callback with the interrupt handler for the associated Signal. If you call register with the same name it will replace the previous callback.
```rust
use addy::{Signal, SIGWINCH};
fn my_func(signal: Signal) {
	/* Does a thing */
}
fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGWINCH)
			.register("print", |_signal| { println!("Screen Resized!"); })?
			.register("my_func", my_func)?
			.enable()?;
	Ok(())
}
```

## Enable
Begins capturing the interrupt and calling any associated callbacks. Most often used after a calls .register() 

Alias of `.resume()`
```rust
 use addy::SIGINT;

 fn main() -> Result<(), addy::Error> {
 	addy::mediate(SIGINT)
				.register("print", |_signal| { println!("Interrupted!"); })?
				.enable()?;
		Ok(())
 }
```

## Remove
Removes a named callback from the associated Signal. If no callback with that name exists, it does nothing.
```rust
use addy::{Signal, SIGWINCH};
fn my_func(signal: Signal) {
	/* Does a thing */
}
fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGWINCH)
			.register("print", |_signal| { println!("Screen Resized!"); })?
			.register("my_func", my_func)?
			.enable()?;
	//-- Later --//
	// Stop calling "print" when the process receives a SIGWINCH signal
	addy::mediate(SIGWINCH).remove("print")?;
	Ok(())
}
```

## Clear
Removes a all callbacks from the associated Signal. Functionally similar
to calling `.ignore()` except you don't need to call `.enable()` if you add
new callbacks later.

```rust
use addy::{Signal, SIGWINCH};

fn my_func(signal: Signal) {
	/* Does a thing */
}

fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGWINCH)
			.register("print", |_signal| { println!("Screen Resized!"); })?
			.register("my_func", my_func)?
			.enable()?;

	//-- Later --//

	// Capture the signal, but stop calling anything
	addy::mediate(SIGWINCH)
			.clear()?
			.register("solo_callback", |_signal| { println!("ALONE!"); })?;

	Ok(())
}
```

## Release 
Removes a all callbacks from the associated Signal and resets the interrupt handler to the default behavior. Funcationally the same as calling `.clear()` and `.default()`
You will need to call `.enable()` again after re-registering callbacks.
```rust
use addy::SIGWINCH;

fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGWINCH)
			.register("print", |_signal| { println!("Screen Resized!"); })?
			.enable()?;

	//-- Later --//

	// Stop capturing the signal
	addy::mediate(SIGWINCH).release()?;

	//-- Later Still --//

	// Start catpuring again
	addy::mediate(SIGWINCH)
			.register("new", |_signal| { println!("New callback!"); })?
			.enable()?;

	Ok(())
}
```

## Ignore
Tells the process to ignore this interrupt. Keeps all your callbacks. Calling `.resume()` will re-enable them.
```rust
use addy::SIGWINCH;

fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGWINCH)
			.register("print", |_signal| { println!("Screen Resized!"); })?
			.enable()?;

	//-- Later --//

	// Ignore the signal
	addy::mediate(SIGWINCH).ignore()?;

	//-- Later Still --//

	// Start catpuring again
	addy::mediate(SIGWINCH).resume()?;

	Ok(())
}
```

## Default
Restore the interrupt handler to the system default. Not all interrupts have a default, and some interrupts default is to be ignored. Keeps all your callbacks. Calling `.resume()` will re-enable them.
```rust
use addy::SIGINT;

fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGINT)
			.register("print", |_signal| { println!("Interrupted!"); })?
			.enable()?;

	//-- Later --//

	// Set the signal to its default
	addy::mediate(SIGINT).default()?;

	//-- Later Still --//

	// Start catpuring again
	addy::mediate(SIGINT).resume()?;

	Ok(())
}
```

## Resume
Resumes capturing the interrupt and calling any associated callbacks. Most often used after a call to .ignore() and .default(). 

Alias of .enable()
```rust
use addy::SIGINT;

fn main() -> Result<(), addy::Error> {
	addy::mediate(SIGINT)
			.register("print", |_signal| { println!("Interrupted!"); })?
			.enable()?;

	//-- Later --//

	// Set the signal to its default
	addy::mediate(SIGINT).default()?;

	//-- Later Still --//

	// Start catpuring and printing "Interrupted!" again
	addy::mediate(SIGINT).resume()?;

	Ok(())
}
```

# Things To Know
I love you and I wish the best for you. No matter what you choose to do, I hope you decide it is worth you time to do it well.

## Addy is Thread Safe!
You can call it from anywhere, at anytime! You can store a SignalHandle (returned from addy::mediate(signal)) in a variable and pass it around.
```rust
use addy::{SIGWINCH, SIGINT};
use std::io::{Read, stdin};
static QUOTE: &'static str = "Look at you, hacker: a pathetic creature of meat \
							  and bone, panting and sweating as you run through \
							  my corridors. How can you challenge a perfect, \
							  immortal machine?";

fn main() -> Result<(), addy::Error> {
	/* When the window resizes */
    addy::mediate(SIGWINCH)
    		.register("hello", |_signal| { println!("Hello, World!"); })?
    		.register("girls", |_signal| { println!("Hello, Girls!"); })?
    		.enable()?;

    /* SIGINT is sent when the user presses Ctrl + C. The default behavior is 
     * to interrupt the program's execution.
    */
    let mut ctrl_c = addy::mediate(SIGINT);
    ctrl_c.register("no_interruptions", |_signal| { println!("{}", QUOTE); })?.enable()?;

    /* Let the user use Ctrl + C to kill the program after 10 seconds */
    std::thread::spawn(move || -> Result<(), addy::Error> {
        std::thread::sleep(std::time::Duration::from_secs(10));
        ctrl_c.default()?;
        Ok(())
    });

    /* Stop saying "Hello, World!" on resize after 5 seconds */
    std::thread::spawn(move || -> Result<(), addy::Error> {
        std::thread::sleep(std::time::Duration::from_secs(5));
        addy::mediate(SIGWINCH).remove("hello")?;
        Ok(())
    });

    /* Capture the input so we don't exit the program immediately */
    let mut buffer = [0; 1];
    loop {
        stdin().read(&mut buffer);
    }

    Ok(())
}
```

## Signals Supported
Not all signals are supported on all platforms/architectures. Which signals
does your platform support? Run: `kill -l` to find out!

* SIGHUP
* SIGINT 
* SIGQUIT
* SIGILL 
* SIGTRAP
* SIGABRT
* SIGBUS 
* SIGFPE 
* SIGKILL
* SIGUSR1
* SIGSEGV
* SIGUSR2
* SIGPIPE
* SIGALRM
* SIGTERM
* SIGSTKF
* SIGCHLD
* SIGCONT
* SIGSTOP
* SIGTSTP
* SIGTTIN
* SIGTTOU
* SIGURG 
* SIGXCPU
* SIGXFSZ
* SIGVTAL
* SIGPROF
* SIGWINC
* SIGIO  
* SIGPWR 
* SIGSYS 
* SIGEMT 
* SIGINFO

## Errors
If the MPSC channel closes, of the Event Loop thread closes, there is no way to recover and any future Addy calls will return an addy::Error.