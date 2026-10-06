// SPDX-License-Identifier: MIT OR Apache-2.0

//! Run inside a delegated test service with MIXOS_CONTAIN_CHILDREN=1.
//! A double-forked child changes session and process group immediately, then
//! remains alive until native cgroup cleanup. This catches adoption-after-exec.

use slots::launch::scope::containment;
use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};

fn main() {
    if std::env::args().any(|arg| arg == "--descendant") {
        unsafe {
            let first = libc::fork();
            assert!(first >= 0);
            if first > 0 {
                libc::_exit(0);
            }
            assert!(libc::setsid() >= 0);
            let second = libc::fork();
            assert!(second >= 0);
            if second > 0 {
                libc::_exit(0);
            }
        }
        println!("{}", std::process::id());
        loop {
            unsafe {
                libc::pause();
            }
        }
    }

    assert!(
        containment::enabled(),
        "set MIXOS_CONTAIN_CHILDREN=1 in a delegated service"
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.arg("--descendant").stdout(Stdio::piped());
    containment::configure_command(&mut command).expect("native pre-exec placement");
    let mut child = command.spawn().expect("launch containment fixture");
    let stdout = child.stdout.take().unwrap();
    let mut ready = libc::pollfd {
        fd: stdout.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(
        unsafe { libc::poll(&mut ready, 1, 5000) } > 0,
        "descendant readiness timeout"
    );
    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    reader.read_line(&mut line).unwrap();
    let pid: i32 = line.trim().parse().expect("grandchild PID");
    assert!(
        child.wait().unwrap().success(),
        "intermediate child exits before cleanup"
    );
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    assert!(pidfd >= 0, "detached grandchild must still be alive");
    containment::kill_all().expect("collect all descendants without a bus");
    let mut exit = libc::pollfd {
        fd: pidfd,
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(
        unsafe { libc::poll(&mut exit, 1, 5000) } > 0,
        "detached grandchild survived cleanup"
    );
    assert_ne!(exit.revents & libc::POLLIN, 0, "pidfd did not report process exit");
    unsafe {
        libc::close(pidfd);
    }
    println!("native containment: double-forked descendant collected");
}
