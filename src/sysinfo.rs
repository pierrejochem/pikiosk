//! System information shown on the kiosk. Everything degrades gracefully when
//! a source is missing (e.g. no /proc when running on a Mac).

use std::fs;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Iface {
    pub name: String,
    pub addrs: Vec<String>,
    pub up: bool,
    pub wireless: bool,
    /// WiFi link quality 0..1 if known
    pub quality: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct SysInfo {
    pub hostname: String,
    pub ifaces: Vec<Iface>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub uptime: Option<Duration>,
    pub temp_c: Option<f32>,
    /// (used, total) in MiB
    pub mem_mb: Option<(u64, u64)>,
    pub load: Option<f32>,
}

impl SysInfo {
    pub fn primary(&self) -> Option<&Iface> {
        self.ifaces
            .iter()
            .filter(|i| i.up && !i.addrs.is_empty())
            .min_by_key(|i| (i.wireless, i.name.clone()))
    }
}

pub struct Monitor {
    info: SysInfo,
    last: Option<Instant>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self { info: SysInfo::default(), last: None }
    }
}

impl Monitor {
    /// Returns current info, refreshing at most once per second.
    pub fn get(&mut self) -> &SysInfo {
        if self.last.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            self.info = collect();
            self.last = Some(Instant::now());
        }
        &self.info
    }
}

/// Interfaces that are noise on a kiosk screen.
fn is_virtual(name: &str) -> bool {
    const PREFIXES: [&str; 12] = ["lo", "ifb", "dummy", "sit", "tunl", "ip6", "gre", "erspan", "veth", "docker", "virbr", "bond"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc == 0 {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let name = String::from_utf8_lossy(&buf[..end]).into_owned();
        if !name.is_empty() {
            return name;
        }
    }
    "kiosk".into()
}

fn collect() -> SysInfo {
    let mut ifaces: Vec<Iface> = Vec::new();
    if let Ok(list) = if_addrs::get_if_addrs() {
        for a in list {
            if a.is_loopback() || is_virtual(&a.name) {
                continue;
            }
            let addr = match a.addr {
                if_addrs::IfAddr::V4(v4) => {
                    let prefix = u32::from(v4.netmask).count_ones();
                    format!("{}/{}", v4.ip, prefix)
                }
                if_addrs::IfAddr::V6(v6) => {
                    // link-local addresses are noise on a kiosk screen
                    if v6.ip.segments()[0] & 0xffc0 == 0xfe80 {
                        continue;
                    }
                    v6.ip.to_string()
                }
            };
            match ifaces.iter_mut().find(|i| i.name == a.name) {
                Some(i) => i.addrs.push(addr),
                None => ifaces.push(Iface { name: a.name.clone(), addrs: vec![addr], up: true, wireless: false, quality: None }),
            }
        }
    }

    // Linux: also list interfaces without addresses, with state and type
    if let Ok(dir) = fs::read_dir("/sys/class/net") {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_virtual(&name) {
                continue;
            }
            let state = read(&format!("/sys/class/net/{name}/operstate")).unwrap_or_default();
            let up = matches!(state.trim(), "up" | "unknown");
            let wireless = fs::metadata(format!("/sys/class/net/{name}/wireless")).is_ok();
            match ifaces.iter_mut().find(|i| i.name == name) {
                Some(i) => {
                    i.up = up;
                    i.wireless = wireless;
                }
                None => ifaces.push(Iface { name, addrs: vec![], up, wireless, quality: None }),
            }
        }
    }
    // sort IPv4 first inside each interface, interfaces by name
    for i in &mut ifaces {
        i.addrs.sort_by_key(|a| a.contains(':'));
    }
    ifaces.sort_by(|a, b| a.name.cmp(&b.name));

    if let Some(w) = read("/proc/net/wireless") {
        for line in w.lines().skip(2) {
            let mut parts = line.split_whitespace();
            let (Some(name), _, Some(link)) = (parts.next(), parts.next(), parts.next()) else { continue };
            let name = name.trim_end_matches(':');
            if let (Some(i), Ok(q)) = (ifaces.iter_mut().find(|i| i.name == name), link.trim_end_matches('.').parse::<f32>()) {
                i.quality = Some((q / 70.0).clamp(0.0, 1.0));
            }
        }
    }

    let gateway = read("/proc/net/route").and_then(|r| {
        r.lines().skip(1).find_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() > 2 && f[1] == "00000000")
                .then(|| u32::from_str_radix(f[2], 16).ok())
                .flatten()
                .map(|g| Ipv4Addr::from(u32::from_be(g)).to_string())
        })
    });

    let dns = ["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"]
        .iter()
        .find_map(|p| read(p))
        .map(|r| {
            r.lines()
                .filter_map(|l| l.strip_prefix("nameserver"))
                .map(|s| s.trim().to_string())
                .filter(|s| s != "127.0.0.53")
                .collect()
        })
        .unwrap_or_default();

    let uptime = read("/proc/uptime")
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .map(Duration::from_secs_f64);

    let temp_c = read("/sys/class/thermal/thermal_zone0/temp").and_then(|s| s.trim().parse::<f32>().ok()).map(|m| m / 1000.0);

    let mem_mb = read("/proc/meminfo").and_then(|m| {
        let field = |k: &str| {
            m.lines()
                .find(|l| l.starts_with(k))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        };
        let total = field("MemTotal:")?;
        let avail = field("MemAvailable:")?;
        Some(((total - avail) / 1024, total / 1024))
    });

    let load = read("/proc/loadavg").and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()));

    SysInfo { hostname: hostname(), ifaces, gateway, dns, uptime, temp_c, mem_mb, load }
}

pub fn format_uptime(d: Duration) -> String {
    let s = d.as_secs();
    let (days, hours, mins) = (s / 86400, s / 3600 % 24, s / 60 % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else {
        format!("{mins} min")
    }
}
