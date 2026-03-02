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

pub fn kill_process_group(pgid: u32, emergency: bool) -> Result<String, String> {
    let neg_pgid = -(pgid as i32);

    if emergency {
        let result = unsafe { libc::kill(neg_pgid, libc::SIGKILL) };
        if result != 0 {
            return Err(format!("Failed to SIGKILL process group {pgid}"));
        }
        return Ok(format!("Process group {pgid} emergency-killed (SIGKILL)"));
    }

    let result = unsafe { libc::kill(neg_pgid, libc::SIGTERM) };
    if result != 0 {
        return Err(format!("Failed to SIGTERM process group {pgid}"));
    }

    std::thread::sleep(std::time::Duration::from_secs(3));

    let check = unsafe { libc::kill(neg_pgid, 0) };
    if check == 0 {
        unsafe {
            libc::kill(neg_pgid, libc::SIGKILL);
        }
        Ok(format!(
            "Process group {pgid} force-killed (SIGKILL after SIGTERM)"
        ))
    } else {
        Ok(format!("Process group {pgid} terminated (SIGTERM)"))
    }
}
