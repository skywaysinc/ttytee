##  TTYTEE - A process that exposes 2 copies of the same TTY.

[![Crates.io](https://img.shields.io/crates/v/ttytee.svg)](https://crates.io/crates/ttytee)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

The initial use case for this crate has been sharing a single GPS device talking through an
USB UART to 2 processes but you can probably use it for sharing UARTs in general.


It had been tested under linux on x86-64, aarch32 and 64 bits. Instructions to compile it
completely statically with musl is explained on the github page: [skywaysinc/ttytee](https://github.com/skywaysinc/ttytee)


![Concept](https://github.com/skywaysinc/ttytee/blob/main/ttytee.svg?raw=true)


The command line help:

```
Usage: ttytee [OPTIONS]

Options:
  -m, --master <MASTER>                              [default: /dev/ttyUSB0]
      --baudrate <BAUDRATE>                          [default: 9600]
      --slave0 <SLAVE0>                              [default: slave0.pty]
      --slave1 <SLAVE1>                              [default: slave1.pty]
      --master-read-timeout <MASTER SERIAL TIMEOUT>  [default: 1000]
      --slave-read-timeout <SLAVE READ TIMEOUT>      [default: 1000]
      --log-path <LOG_PATH>
  -h, --help                                         Print help
  -V, --version                                      Print version
```
*master* is the path pointing to the real device.

*slave0* and *slave1* will be PTY devices that will expose the same data as master.


*Very important note*: The use case for this program is real time so if one of the slave
cannot catch up its data from the PTY will be erased to keep up with real time and the other
slave won't be affected. It is set by the slave-read-timeout.


Writes from the slaves are not supported.
//!
