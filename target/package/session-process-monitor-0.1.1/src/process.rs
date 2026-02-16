pub fn terminate_process(pid: u32) -> Result<String, String> {
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if result != 0 {
        return Err(format!("Failed to send SIGTERM to {}", pid));
    }

    std::thread::sleep(std::time::Duration::from_secs(3));

    let check = unsafe { libc::kill(pid as i32, 0) };
    if check == 0 {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        Ok(format!("Process {} force-killed (SIGKILL)", pid))
    } else {
        Ok(format!("Process {} terminated (SIGTERM)", pid))
    }
}
