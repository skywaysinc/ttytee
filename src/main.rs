//! A process that exposes 2 copies of the same TTY.
//!
//! The initial use case for this crate has been sharing a single GPS device talking through an
//! USB UART to 2 processes but you can probably use it for sharing UARTs in general.
//!
//! It had been tested under linux on x86-64, aarch32 and 64 bits. Instructions to compile it
//! completely statically with musl is explained on the github page: [skywaysinc/ttytee](https://github.com/skywaysinc/ttytee)
//!
//! ![Chain](https://github.com/skywaysinc/ttytee/ttytee.svg)
//!
//! The command line help:
//!
//! ```
//! Usage: ttytee [OPTIONS]
//!
//! Options:
//!   -m, --master <MASTER>                              [default: /dev/ttyUSB0]
//!       --baudrate <BAUDRATE>                          [default: 9600]
//!       --slave0 <SLAVE0>                              [default: slave0.pty]
//!       --slave1 <SLAVE1>                              [default: slave1.pty]
//!       --master-read-timeout <MASTER SERIAL TIMEOUT>  [default: 1000]
//!       --slave-read-timeout <SLAVE READ TIMEOUT>      [default: 1000]
//!       --log-path <LOG_PATH>
//!   -h, --help                                         Print help
//!   -V, --version                                      Print version
//! ```
//! *master* is the path pointing to the real device.
//!
//! *slave0* and *slave1* will be PTY devices that will expose the same data as master.
//!
//!
//! *Very important note*: The use case for this program is real time so if one of the slave
//! cannot catch up its data from the PTY will be erased to keep up with real time and the other
//! slave won't be affected. It is set by the slave-read-timeout.
//!
//!
//! Writes from the slaves are not supported.
//!

use clap::{arg, Parser};
use log::{debug, error, info, warn};
use serialport::{ClearBuffer, SerialPort, TTYPort};
use simplelog::{
    ColorChoice, CombinedLogger, Config, LevelFilter, SharedLogger, TermLogger, TerminalMode,
    WriteLogger,
};
use std::fs::{remove_file, File};
use std::io::{Read, Write};
use std::os::unix::fs;
use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
use std::{thread, time};

const SLAVE0: &str = "slave0.pty";
const SLAVE1: &str = "slave1.pty";
const DEFAULT_MASTER: &str = "/dev/ttyUSB0";

const MASTER_SERIAL_TIMEOUT_MS: u64 = 1000;

// Usually GPSes are a 9600, default to this.
const DEFAULT_BAUDRATE: u32 = 9600;

// Consider any lines older than this duration stale and worth taking out of the TTY buffer.
const SLAVE_READ_TIMEOUT_MS: u64 = 1000;

// Just an arbitrary wait time just in case an error keeps on repeating forever.
const ANTI_HOTLOOP: Duration = Duration::from_millis(500);

// declare the command line format
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    // TTY to read from
    #[arg(short, long, default_value = DEFAULT_MASTER, value_name = "MASTER")]
    master: PathBuf,
    // Baudrate to read the master from.
    #[arg(long, default_value_t = DEFAULT_BAUDRATE, value_name = "BAUDRATE")]
    baudrate: u32,
    // First PTY that will replicate MASTER.
    #[arg(long, default_value = SLAVE0, value_name = "SLAVE0")]
    slave0: PathBuf,
    // Second PTY that will replicate MASTER.
    #[arg(long, default_value = SLAVE1, value_name = "SLAVE1")]
    slave1: PathBuf,
    // Timeout in ms after the main read on the master TTY timeouts.
    #[arg(long, default_value_t = MASTER_SERIAL_TIMEOUT_MS, value_name = "MASTER SERIAL TIMEOUT")]
    master_read_timeout: u64,
    // Timeout in ms after which any lines older than this will be considered stale and removed.
    #[arg(long, default_value_t = SLAVE_READ_TIMEOUT_MS, value_name = "SLAVE READ TIMEOUT")]
    slave_read_timeout: u64,
    #[arg(long, value_name = "LOG_PATH")]
    log_path: Option<PathBuf>,
}

/// Create a combined logger between the console and a log file.
///
/// # Arguments
///
/// * `log_path`: Optionally a log path to create a log file.
///
/// returns: ()
///
fn init_logger(log_path: &Option<PathBuf>) {
    let mut loggers: Vec<Box<dyn SharedLogger>> = vec![
        // Let it at Debug as we compile out the Debug level on release.
        TermLogger::new(
            LevelFilter::Debug,
            Config::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ),
    ];
    if log_path.is_some() {
        loggers.push(WriteLogger::new(
            LevelFilter::Info,
            Config::default(),
            File::create(log_path.as_ref().unwrap()).unwrap(),
        ))
    }
    // configure the logger.
    CombinedLogger::init(loggers).unwrap();
}

fn main() {
    // parse the command line
    let args = Args::parse();
    init_logger(&args.log_path);
    let process_exit_code = ttytee(&args, &AtomicBool::new(true));
    exit(process_exit_code);
}

/// Copy a buffer from a master TTY to a slave.
///
/// # Arguments
///
/// * `master`:  the master you want to copy the line from.
/// * `slave`:  the slave you want to copy into.
/// * `last_good_read`:  the last recorded time you know the client has properly read the stream.
/// * `buffer`:  the buffer itself.
/// * `slave_read_timeout`:  what is the maximum time you allow the client to read the line from the slave tty.
///
/// returns: Result<SystemTime, Error> the new last_good_read from this client.
///
fn new_buffer_to_client(
    master: &mut TTYPort,
    slave: &TTYPort,
    mut last_good_read: SystemTime,
    buffer: &[u8],
    read_len: usize,
    slave_read_timeout: Duration,
) -> Result<SystemTime, serialport::Error> {
    let duration_since_last_known_read = last_good_read
        .elapsed()
        .expect("Could not calculate elapsed time");
    if duration_since_last_known_read > slave_read_timeout {
        warn!("Cleared stale buffer from {}.", slave.name().unwrap());
        last_good_read = SystemTime::now();
        master.clear(ClearBuffer::All)?;
        slave.clear(ClearBuffer::All)?;
    }
    let left_in_buffer = slave.bytes_to_read()?;
    if left_in_buffer < 2048 {
        last_good_read = SystemTime::now();
        match master.write(&buffer[..read_len]) {
            Ok(nbchar) => {
                debug!("Wrote {} chrs to {:?}.", nbchar, master);
                return Ok(last_good_read);
            }
            Err(err) => {
                warn!("Failed to write on master {:?}: {}.", master, err);
            }
        }
    } else {
        debug!(
            "Slave {} could not keep up, we skipped writting in their buffer.",
            slave.name().unwrap()
        );
    }
    Ok(last_good_read)
}

struct SelfCleaningSymlink {
    path: PathBuf,
}

impl SelfCleaningSymlink {
    /// Create a symlink that will clean up at drop time.
    ///
    /// # Arguments
    ///
    /// * `from`: source of the link
    /// * `to`: destination of the link (where it will be created).
    ///
    /// returns: SelfCleaningSymlink
    ///
    /// # Examples
    ///
    /// ```
    ///     fn myfunc() {
    ///         let _link = SelfCleaningSymlink::create("/from/real_file", "/to/symlink");
    ///         // Note: it needs to be binding so use _name not _.
    ///         //
    ///         //
    ///         // ... do things.
    ///         //
    ///         //
    ///         //  <- here it will remove /to/symlink.
    ///     }
    /// ```
    pub fn create(from: &PathBuf, to: &PathBuf) -> Self {
        remove_file(to).ok(); // ok to ignore if the links are not there.
        match fs::symlink(from, to) {
            Err(err) => {
                error!(
                    "Could not create the symlink from {:?} -> {:?}: {:?}.",
                    from, to, err
                );
            }
            Ok(_) => {
                debug!("Symlink {:?} -> {:?} created successfully.", from, to);
            }
        }
        Self { path: to.clone() }
    }
}

impl Drop for SelfCleaningSymlink {
    fn drop(&mut self) {
        remove_file(&self.path).unwrap(); // for the cleanup, the link should be there!
        debug!("Symlink {:?} cleaned up.", self.path);
    }
}

// Split out the inner logic so testing is easier.
fn ttytee(args: &Args, running: &AtomicBool) -> i32 {
    // returns a process error code. 0 if everything went right.
    let serial_timeout: time::Duration = time::Duration::from_millis(args.master_read_timeout);
    let slave_read_timeout: Duration = Duration::from_millis(args.slave_read_timeout);
    info!("ttytee is starting...");

    let tty_name = args.master.to_str().unwrap();
    // Creates a serial port builder. Defaults are N81 with no timeout.
    let serial = &serialport::new(tty_name, args.baudrate);
    let mut tty = match TTYPort::open(serial) {
        Ok(tty) => tty,
        Err(err) => {
            error!("Could not open the given port {:?}: {}", serial, err);
            return 1;
        }
    };

    // prevent somebody else to read from the same real device.
    tty.set_exclusive(true)
        .expect("Could not get exclusive access to the serial port.");

    // A fairly large timeout as the data is coming slowly.
    tty.set_timeout(serial_timeout)
        .expect("Could not set a read timeout on the serial port.");

    let (mut master0_tty, slave0_tty) =
        TTYPort::pair().expect("Could not create the first master slave");
    let (mut master1_tty, slave1_tty) =
        TTYPort::pair().expect("Could not create the second master slave");

    let real_slave0_tty_path = PathBuf::from(slave0_tty.name().unwrap());
    let real_slave1_tty_path = PathBuf::from(slave1_tty.name().unwrap());
    let _scs0 = SelfCleaningSymlink::create(&real_slave0_tty_path, &args.slave0);
    let _scs1 = SelfCleaningSymlink::create(&real_slave1_tty_path, &args.slave1);

    let now = SystemTime::now();
    let (mut last_good_read0, mut last_good_read1) = (now, now);

    let mut buffer_bytes: [u8; 4096] = [0; 4096];
    while running.load(Ordering::Relaxed) {
        match tty.read(&mut buffer_bytes) {
            Ok(0) => {
                warn!("EOF ... try again.");
                thread::sleep(ANTI_HOTLOOP);
            }
            Ok(read_len) => {
                debug!("Received from {}: {} bytes.", tty_name, read_len);

                // send the line to each client.
                match new_buffer_to_client(
                    &mut master0_tty,
                    &slave0_tty,
                    last_good_read0,
                    &buffer_bytes,
                    read_len,
                    slave_read_timeout,
                ) {
                    Ok(new_last_good_read) => {
                        last_good_read0 = new_last_good_read;
                    }
                    Err(err) => {
                        // IO error, try to continue anyway.
                        warn!("IO error on master/slave0 {}.", err);
                        thread::sleep(ANTI_HOTLOOP);
                    }
                };

                match new_buffer_to_client(
                    &mut master1_tty,
                    &slave1_tty,
                    last_good_read1,
                    &buffer_bytes,
                    read_len,
                    slave_read_timeout,
                ) {
                    Ok(new_last_good_read) => {
                        last_good_read1 = new_last_good_read;
                    }
                    Err(err) => {
                        // IO error, try to continue anyway.
                        warn!("IO error on master/slave1 {}.", err);
                        thread::sleep(ANTI_HOTLOOP);
                    }
                };
            }
            Err(err) => {
                warn!("Error reading from serial port: {}. Trying again.", err);
                thread::sleep(ANTI_HOTLOOP);
            }
        }
    }
    info!("ttytee is ending with no error.");
    0
}

#[cfg(test)]
mod tests {
    use crate::{init_logger, ttytee, Args};
    use log::debug;
    use serialport::{SerialPort, TTYPort};
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::Duration;

    #[ctor::ctor]
    fn init() {
        init_logger(&None);
    }

    fn setup_tty_counter() -> TTYPort {
        let mut buffer: [u8; 1000] = [0; 1000];
        let (mut master, fake_gps) = TTYPort::pair().unwrap();
        thread::spawn(move || {
            for i in 0..9 {
                debug!("====> Writing {}...", i);
                let chr: u8 = format!("{}", i).as_bytes()[0];
                for j in 0..buffer.len() {
                    buffer[j] = chr;
                }
                thread::sleep(Duration::from_millis(500));
                master.write(&buffer).unwrap();
            }
        });
        fake_gps
    }

    fn start_async_ttytee(args: Args, running: &Arc<AtomicBool>) -> JoinHandle<()> {
        let running_ref = Arc::clone(running);
        thread::spawn(move || {
            ttytee(&args, &running_ref);
        })
    }

    #[test]
    fn test_non_existent_tty() {
        let args = Args {
            master: PathBuf::from("/tmp/fake_master"),
            slave0: PathBuf::from("/tmp/slave0"),
            slave1: PathBuf::from("/tmp/slave1"),
            baudrate: Default::default(),
            master_read_timeout: Default::default(),
            slave_read_timeout: Default::default(),
            log_path: Default::default(),
        };
        assert_eq!(ttytee(&args, &AtomicBool::new(false)), 1);
    }

    #[test]
    fn test_leakiness() {
        let original_tty = setup_tty_counter();
        debug!(" tty = {:?}", original_tty);

        let running = Arc::new(AtomicBool::new(true));
        let slave0 = PathBuf::from("/tmp/slave0");
        let args = Args {
            master: PathBuf::from(original_tty.name().unwrap()),
            slave0: slave0.clone(),
            slave1: PathBuf::from("/tmp/slave1"),
            baudrate: Default::default(),
            master_read_timeout: Default::default(),
            slave_read_timeout: 100,
            log_path: None,
        };
        let t = start_async_ttytee(args, &running);
        while !slave0.exists() {
            debug!("Waiting for ttytee to start up... ");
            thread::sleep(Duration::from_millis(500));
        }

        let mut serial_port_builder = serialport::new("/tmp/slave0", 115200);
        serial_port_builder = serial_port_builder.timeout(Duration::from_secs(5));
        let mut slave0 = TTYPort::open(&serial_port_builder).unwrap();

        let mut first_buffer: [u8; 100] = [0; 100];
        let mut bytes_read = slave0.read(&mut first_buffer).unwrap();
        assert_ne!(bytes_read, 0);
        debug!(
            "----> Read {} bytes on the first buffer. chr == {}",
            first_buffer.len(),
            first_buffer[0]
        );
        thread::sleep(Duration::from_secs(2)); // be sure we miss some

        let mut second_buffer: [u8; 100] = [0; 100];
        bytes_read = slave0.read(&mut second_buffer).unwrap();
        assert_ne!(bytes_read, 0);
        debug!(
            "----> Read {} bytes on the second buffer. chr == {}",
            second_buffer.len(),
            second_buffer[0]
        );
        // unsure that ttytee is "leaky" ie. drops the lines if the client cannot follow.
        assert_ne!(first_buffer[0] + 1, second_buffer[0]);
        debug!("Ending the ttytee thread ...");
        running.store(false, Ordering::Relaxed);
        t.join().expect("Could not join with the ttytee thread.");
        debug!("Done.");
    }
}
