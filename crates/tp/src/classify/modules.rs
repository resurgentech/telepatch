//! Module name -> category and proposed telepatch primitive.

/// Category display order in the report.
pub const CATEGORY_ORDER: &[&str] = &[
    "pkg", "repo", "file", "sh", "svc", "user", "fetch", "git", "fact", "control", "other",
];

/// The module name without its collection prefix (`ansible.builtin.apt` -> `apt`).
pub fn short(module: &str) -> &str {
    module.rsplit('.').next().unwrap_or(module)
}

/// `(category, primitive)` for a module, or `None` when telepatch has no
/// mapping for it (the task is then listed as unclassified).
pub fn info(module: &str) -> Option<(&'static str, &'static str)> {
    Some(match short(module) {
        "apt" | "dnf" | "yum" | "pacman" | "package" | "apk" | "zypper" => ("pkg", "pkg"),
        "pip" => ("pkg", "pkg::pip"),
        "apt_key" | "apt_repository" | "yum_repository" | "rpm_key" | "deb822_repository"
        | "dnf_repository" => ("repo", "repo"),
        "file" | "template" | "copy" | "lineinfile" | "replace" | "blockinfile" => ("file", "file"),
        "shell" | "command" | "raw" | "script" => ("sh", "sh"),
        "systemd" | "systemd_service" | "service" | "sysvinit" => ("svc", "svc"),
        "user" | "authorized_key" => ("user", "user"),
        "group" => ("user", "group (tier two)"),
        "get_url" | "uri" => ("fetch", "fetch"),
        "unarchive" => ("fetch", "unarchive (tier two)"),
        "git" => ("git", "git (tier two)"),
        "stat" | "find" | "getent" | "setup" | "package_facts" | "service_facts" => {
            ("fact", "fact (exists, stat, user_info, ...)")
        }
        "include_tasks" | "import_tasks" | "include" | "include_role" | "import_role"
        | "set_fact" | "debug" | "fail" | "assert" | "meta" => ("control", "Rust control flow"),
        "reboot" => ("other", "reboot (tier two)"),
        "mount" => ("other", "mount (tier two)"),
        "firewalld" => ("other", "firewall (tier two)"),
        _ => return None,
    })
}
