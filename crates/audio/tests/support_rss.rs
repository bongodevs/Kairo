pub(crate) fn rss() -> u64 {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let Ok(mut system) = sysinfo::System::new() else {
        return 0;
    };
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map_or(0, sysinfo::Process::memory)
}
