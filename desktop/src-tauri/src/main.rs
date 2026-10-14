// P7 桌面壳 → 附着优先模式（用户拍板改造）：
// 1. 启动先探测外部 hub（默认 http://127.0.0.1:7800，env AGENT_HUB_ATTACH_URL 覆盖），
//    GET /api/drivers 200 即存活 → 直接开 WebView 指向该地址，不拉 sidecar
// 2. 探测失败 → 回退现行 sidecar 流程（7801），行为与改造前完全一致
// 3. 退出语义分离（安全关键）：附着模式关窗 = 壳自身退出，绝不向外部 hub 发任何
//    信号——连坐 killpg 只许对着自己 spawn 的 sidecar 进程组（SERVER_PGID=0 时
//    kill_server_group/on_signal 天然 no-op，结构上保证不会误伤）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{self, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
use tauri::Manager;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const SERVER_ADDR: &str = "127.0.0.1:7801";
const SERVER_URL: &str = "http://127.0.0.1:7801";
/// 附着目标缺省（外部 hub；env `AGENT_HUB_ATTACH_URL` 覆盖，v1 不落盘）
const ATTACH_URL_DEFAULT: &str = "http://127.0.0.1:7800";
/// 附着探测超时（连接与读各 3s）
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// server 就绪等待上限（超时弹错退出）
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// sidecar 进程组 id；信号处理器异步安全读取（0=未启动）
static SERVER_PGID: AtomicI32 = AtomicI32::new(0);

/// SIGTERM/SIGINT（外部 kill / 注销 / Activity Monitor 退出）时连坐杀 server：
/// Drop 只在正常退出路径运行，信号路径必须这里兜底（否则 sidecar 成孤儿霸占 7800）
extern "C" fn on_signal(_sig: libc::c_int) {
    let pgid = SERVER_PGID.load(Ordering::SeqCst);
    if pgid > 0 {
        // SAFETY: killpg 传已注册的进程组 id；errno 忽略（组可能已退出）
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
    }
    // SAFETY: _exit 异步信号安全；server 已收到 SIGTERM，不等待收尸
    unsafe {
        libc::_exit(143);
    }
}

/// 注册外部终止信号的连坐兜底（仅 unix；失败静默——最坏情形退回现状）
#[cfg(unix)]
fn install_signal_guards() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
}

#[cfg(not(unix))]
fn install_signal_guards() {}

/// GUI .app 环境的 PATH 只有系统默认（/usr/bin:/bin:/usr/sbin:/sbin），
/// 找不到 claude（~/.local/bin）/omp（~/.bun/bin）等用户工具 → server spawn ENOENT。
/// 在继承 PATH 基础上补常见安装位（目录存在才拼 + 去重）。
fn extended_path(inherited: &str, home: &str) -> String {
    let candidates = [
        "/usr/local/bin",
        "/usr/local/sbin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        &format!("{home}/.local/bin"),
        &format!("{home}/.bun/bin"),
        &format!("{home}/.cargo/bin"),
    ];
    let mut parts: Vec<String> = inherited
        .split(':')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    for c in candidates {
        if std::path::Path::new(c).is_dir() && !parts.iter().any(|p| p == c) {
            parts.push(c.to_string());
        }
    }
    parts.join(":")
}

/// sidecar server 子进程；Drop 连坐清除（镜像 drivers/acp/mod.rs 的 process_group + killpg 模板）
struct ServerChild(Child);

impl ServerChild {
    fn spawn() -> io::Result<Self> {
        // sidecar 与壳同目录（.app/Contents/MacOS/server）；裸跑/测试可用 AGENT_HUB_SERVER_BIN 覆盖
        let exe = std::env::var("AGENT_HUB_SERVER_BIN").unwrap_or_else(|_| {
            std::env::current_exe()
                .expect("无法定位自身可执行文件")
                .parent()
                .expect("无可执行目录")
                .join("server")
                .to_string_lossy()
                .into_owned()
        });
        let mut cmd = Command::new(exe);
        // 桌面壳固定 loopback 免认证形态：显式清掉 LAN 开关，防外层 env 误带入
        cmd.env_remove("AGENT_HUB_ALLOW_LAN");
        // 固定绑 7801：7800 是 web preview/frp 的 LAN 端口，两边共存互不抢
        cmd.env("AGENT_HUB_BIND", SERVER_ADDR);
        // server 及其 fork 的 claude/omp 等工具依赖用户 PATH；GUI 环境默认 PATH 不含
        // ~/.local/bin、~/.bun/bin 等 → 注入扩展 PATH（见 extended_path 单测）
        let path = extended_path(
            &std::env::var("PATH").unwrap_or_default(),
            &std::env::var("HOME").unwrap_or_default(),
        );
        cmd.env("PATH", path);
        #[cfg(unix)]
        // 安全模板 §6：独立进程组 → killpg 连坐（server 自己 fork 的 claude/omp 一并清除）
        cmd.process_group(0);
        let child = ServerChild(cmd.spawn()?);
        #[cfg(unix)]
        SERVER_PGID.store(child.0.id() as i32, Ordering::SeqCst);
        Ok(child)
    }

    /// 端口就绪轮询；期间发现 server 已退出（如 7800 被占 bind 失败）立即失败，不空等
    fn wait_port(&mut self, timeout: Duration) -> bool {
        let addr: SocketAddr = SERVER_ADDR.parse().expect("bad SERVER_ADDR");
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return false;
            }
            if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// SIGTERM → 500ms 宽限 → SIGKILL（同步版 reap，对齐 acp::reap 语义）
    fn kill(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return; // 已退出（含启动即败），无需补刀
        }
        let pid = self.0.id() as i32;
        // SAFETY: killpg 传一个已存在进程组 id；errno 忽略（组可能已退出）
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
        let grace = Instant::now() + Duration::from_millis(500);
        while Instant::now() < grace {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // SAFETY: 宽限期满强制连坐
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
        let _ = self.0.wait(); // reap 防 zombie
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 附着探测的状态行判定（独立成函数供单测）。
fn status_ok(head: &str) -> bool {
    head.starts_with("HTTP/") && head.split_whitespace().nth(1) == Some("200")
}

/// 附着探测（只读）：GET `<attach_url>/api/drivers` 收到 200 = 外部 hub 存活。
/// 手拼最小 HTTP/1.0 GET——目标恒为自家 server（无重定向/分块，契约固定），
/// 零新增依赖；https 目标无法裸 TCP 握手 → 探测失败 → 回退 sidecar（v1 明示限制）。
/// 任何失败一律 false（fail-soft），绝不触碰探测目标的其他端口/进程。
fn probe_alive(attach_url: &str) -> bool {
    let host_port = attach_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or_default();
    if host_port.is_empty() {
        return false;
    }
    let Some(addr) = host_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.find(|_| true))
    else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = s.set_write_timeout(Some(PROBE_TIMEOUT));
    let req =
        format!("GET /api/drivers HTTP/1.0\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.take(256).read_to_end(&mut buf); // 只需要状态行，256B 上限足够
    status_ok(&String::from_utf8_lossy(&buf))
}

/// 启动失败弹原生错误框后退出（rfd 同步 NSAlert，退出码 1）
fn fatal(msg: &str) -> ! {
    kill_server_group(); // 超时等失败路径下 sidecar 可能仍挂着，退出前连坐（附着模式 PGID=0 天然 no-op）
    rfd::MessageDialog::new()
        .set_title("Claude View")
        .set_level(rfd::MessageLevel::Error)
        .set_description(msg)
        .show();
    std::process::exit(1);
}

/// 按进程组连坐杀 server（SIGTERM；幂等，可在任意退出路径调用）。
/// tauri 的 exit 最终走 std::process::exit，不跑 Drop——所有正常退出必须显式走这里。
/// 附着模式 SERVER_PGID 恒 0 → 本函数天然 no-op，绝不向外部 hub 发信号。
fn kill_server_group() {
    let pgid = SERVER_PGID.load(Ordering::SeqCst);
    if pgid > 0 {
        // SAFETY: killpg 传已注册的进程组 id；errno 忽略（组可能已退出）
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
    }
}

fn main() {
    install_signal_guards();
    // —— 附着优先：外部 hub 存活 → 直连；否则回退 sidecar（现行流程）——
    let attach_url =
        std::env::var("AGENT_HUB_ATTACH_URL").unwrap_or_else(|_| ATTACH_URL_DEFAULT.to_string());
    let (target_url, mut server) = if probe_alive(&attach_url) {
        eprintln!(
            "[shell] attach mode → {attach_url}（外部 hub 存活：不拉 sidecar；退出绝不触碰外部 hub）"
        );
        (attach_url, None)
    } else {
        eprintln!("[shell] attach probe failed → fallback sidecar mode → {SERVER_URL}");
        let mut server =
            ServerChild::spawn().unwrap_or_else(|e| fatal(&format!("server 启动失败：{e}")));
        if !server.wait_port(STARTUP_TIMEOUT) {
            fatal(&format!(
                "server 未能在 15 秒内就绪（{SERVER_ADDR} 被占用或启动异常）。\
                 如已有 Claude View 网页版在跑，请先退出后再启动桌面版。"
            ));
        }
        (SERVER_URL.to_string(), Some(server))
    };
    tauri::Builder::default()
        .setup(move |app| {
            let win = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(target_url.parse().expect("bad target url")),
            )
            .title("Claude View")
            .inner_size(1280.0, 800.0)
            .min_inner_size(880.0, 560.0)
            .on_page_load(|_webview, payload| {
                // 运行期诊断：页面加载进度进 stderr（.app 场景不可见，排查时直跑二进制用）
                eprintln!("[shell] page_load {:?} {}", payload.event(), payload.url());
            })
            .build()?;
            win.show()?;
            win.set_focus()?;
            eprintln!("[shell] window created+shown");
            Ok(())
        })
        // 关窗即退出（附着 = 壳自身退出；sidecar = 连坐，见 Exit 分支）
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.app_handle().exit(0);
            }
        })
        .build(tauri::generate_context!())
        .expect("tauri 应用构建失败")
        .run(move |_app, event| {
            // tauri 退出（关窗/Cmd+Q/AppleEvent）前必经 Exit 事件：sidecar 模式显式连坐，
            // 不能依赖 Drop（tauri 的 exit 终点 std::process::exit 不运行析构）。
            // 附着模式 server=None 且 SERVER_PGID=0 → 两个清除步骤均 no-op，
            // 结构上保证绝不向外部 hub 发 kill/信号。
            if let tauri::RunEvent::Exit = event {
                kill_server_group();
                if let Some(s) = server.as_mut() {
                    s.kill();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_path_appends_existing_dirs_and_dedupes() {
        // /usr/bin 一定存在；重复项只保留一次；不存在的目录被跳过
        let out = extended_path("/usr/bin:/opt/homebrew/bin", "/home/test");
        let parts: Vec<&str> = out.split(':').collect();
        assert_eq!(parts[0], "/usr/bin"); // 继承 PATH 保序在前
        assert_eq!(
            parts.iter().filter(|p| **p == "/opt/homebrew/bin").count(),
            1
        );
        assert!(parts.contains(&"/usr/local/bin"));
        assert!(!parts.iter().any(|p| p.contains(".cargo"))); // /home/test/.cargo/bin 不存在
        assert!(out.split(':').count() >= 3);
    }

    #[test]
    fn extended_path_keeps_inherited_order() {
        let out = extended_path("/usr/bin:/bin", "/home/test");
        assert!(out.starts_with("/usr/bin:/bin:"));
    }

    #[test]
    fn status_ok_accepts_200_only() {
        assert!(status_ok("HTTP/1.0 200 OK"));
        assert!(status_ok(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json"
        ));
        assert!(!status_ok("HTTP/1.1 404 Not Found"));
        assert!(!status_ok("HTTP/1.1 500 Internal Server Error"));
        assert!(!status_ok("garbage"));
        assert!(!status_ok(""));
    }

    /// probe 集成：本地 TcpListener 分别扮演 200 hub 与 404 服务，验证真套接字路径。
    #[test]
    fn probe_alive_true_for_200_and_false_for_404_or_dead() {
        let serve_with = |status: &'static str| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                if let Ok((s, _)) = listener.accept() {
                    let mut s = s;
                    let _ = s.write_all(
                        format!("HTTP/1.0 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
                    );
                }
            });
            addr
        };
        let ok = serve_with("200 OK");
        assert!(probe_alive(&format!("http://127.0.0.1:{}", ok.port())));
        let nf = serve_with("404 Not Found");
        assert!(!probe_alive(&format!("http://127.0.0.1:{}", nf.port())));
        // 死端口（3s 探测超时语义下快速失败：连接拒绝）
        assert!(!probe_alive("http://127.0.0.1:1"));
        // 残缺 URL fail-soft
        assert!(!probe_alive("http://"));
    }
}
