//! SSH 进程参数构造共享模块（agent-hub-settings 任务 D2）：隧道转发（tunnel.rs）
//! 与远端命令（远程探测 / 安装执行）共用同一份连接参数构造——**禁两处漂移**。
//! 密码认证走 `sshpass -e` + `SSHPASS` 环境变量（密码不进 argv 不落盘）。

use std::process::Stdio;

use anyhow::{anyhow, Result};
use tokio::process::Command;

use crate::instances::{InstanceConfig, SshAuth};

/// 连接参数（不含 `-N`/`-L` 转发，也不含远端命令）——destination 恒在最后。
/// 参数与顺序逐条对齐原 `tunnel.rs::build_command`，抽这里只为复用。
pub fn ssh_args(inst: &InstanceConfig) -> Result<Vec<String>> {
    let ssh = inst
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow!("ssh-tunnel 实例缺 ssh 参数"))?;

    let mut args = vec![
        "-o".to_string(),
        "ExitOnForwardFailure=yes".into(),
        "-o".to_string(),
        "ServerAliveInterval=15".into(),
        "-o".to_string(),
        "ServerAliveCountMax=3".into(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".into(),
    ];
    match ssh.auth {
        SshAuth::KeyPath => {
            let kp = ssh
                .key_path
                .as_ref()
                .ok_or_else(|| anyhow!("key-path 认证缺密钥路径"))?;
            args.push("-i".into());
            args.push(kp.clone());
        }
        SshAuth::Authsock | SshAuth::Password => {}
    }
    if ssh.port != 22 {
        args.push("-p".into());
        args.push(ssh.port.to_string());
    }
    args.push(format!("{user}@{host}", user = ssh.user, host = ssh.host));
    Ok(args)
}

/// 完整 ssh 进程：
/// - `extra`：隧道转发等附加选项（插在连接参数之前，隧道行为与原实现逐条一致）
/// - `remote_cmd`：远端要执行的命令（`None` = 只建隧道，输出丢弃；有命令则捕获
///   stdout/stderr 供调用方判定）
pub fn command(
    inst: &InstanceConfig,
    extra: &[String],
    remote_cmd: Option<&str>,
) -> Result<Command> {
    let ssh = inst
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow!("ssh-tunnel 实例缺 ssh 参数"))?;

    let mut args = extra.to_vec();
    args.extend(ssh_args(inst)?);
    if let Some(c) = remote_cmd {
        args.push(c.to_string());
    }

    // 密码认证：前置 sshpass（探测缺失 → 引导错误；不阻塞证书/authsock 路径）。
    // 凭据只经 SSHPASS 环境变量传递，不进 argv、不进日志。
    let (mut cmd, pass) = if ssh.auth == SshAuth::Password {
        let pass = ssh.password.as_deref().unwrap_or("");
        if pass.is_empty() {
            anyhow::bail!("密码认证需配置密码");
        }
        if !sshpass_available() {
            anyhow::bail!(
                "密码认证需要 sshpass（本机未安装）。请安装 sshpass 或改用证书路径 / authsock 认证"
            );
        }
        let mut sp = Command::new("sshpass");
        sp.arg("-e").arg("ssh");
        (sp, Some(pass))
    } else {
        (Command::new("ssh"), None)
    };

    cmd.args(&args)
        .process_group(0) // 独立进程组，exit 时 killpg 连坐
        .stdin(Stdio::null());
    if remote_cmd.is_some() {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    if ssh.auth == SshAuth::Authsock {
        if let Ok(v) = std::env::var("SSH_AUTH_SOCK") {
            cmd.env("SSH_AUTH_SOCK", v);
        }
    }
    if let Some(pass) = pass {
        cmd.env("SSHPASS", pass);
    }
    Ok(cmd)
}

pub fn sshpass_available() -> bool {
    // 探测 PATH 中是否存在 sshpass 二进制（只判存，不执行）
    std::process::Command::new("which")
        .arg("sshpass")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::{InstanceMode, SshConfig};

    fn inst(auth: SshAuth, key: Option<&str>, port: u16) -> InstanceConfig {
        InstanceConfig {
            id: "i1".into(),
            name: "x".into(),
            mode: InstanceMode::SshTunnel,
            url: None,
            token: None,
            ssh: Some(SshConfig {
                host: "example.com".into(),
                port,
                user: "u".into(),
                auth,
                key_path: key.map(String::from),
                password: None,
            }),
            remote_port: Some(7800),
            local_port: None,
        }
    }

    /// 共享化零回归：连接参数的内容与顺序（destination 恒在最后）
    #[test]
    fn ssh_args_order_and_content() {
        let a = ssh_args(&inst(SshAuth::KeyPath, Some("/k/id"), 2222)).unwrap();
        assert_eq!(
            a,
            vec![
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-i",
                "/k/id",
                "-p",
                "2222",
                "u@example.com",
            ]
        );
        // 默认端口不加 -p；authsock 不加 -i
        let b = ssh_args(&inst(SshAuth::Authsock, None, 22)).unwrap();
        assert!(!b.contains(&"-p".to_string()));
        assert!(!b.contains(&"-i".to_string()));
        // key-path 缺证书路径 → 错
        assert!(ssh_args(&inst(SshAuth::KeyPath, None, 22)).is_err());
        // 无 ssh 配置 → 错
        let mut c = inst(SshAuth::Authsock, None, 22);
        c.ssh = None;
        assert!(ssh_args(&c).is_err());
    }

    /// 附加选项插在连接参数之前 → 隧道 argv 与原实现逐条一致
    #[test]
    fn extra_prepended_before_connection_args() {
        let i = inst(SshAuth::Authsock, None, 22);
        let mut args = vec![
            "-N".to_string(),
            "-L".to_string(),
            "127.0.0.1:1:127.0.0.1:2".into(),
        ];
        args.extend(ssh_args(&i).unwrap());
        assert_eq!(&args[..3], &["-N", "-L", "127.0.0.1:1:127.0.0.1:2"]);
        assert_eq!(args.last().unwrap(), "u@example.com");
    }

    /// 密码凭据不进 argv（sshpass -e + SSHPASS 环境变量传递）
    #[test]
    fn password_never_in_argv() {
        if !sshpass_available() {
            return; // 本机无 sshpass → command 直接引导性报错，另见 tunnel 测试
        }
        let mut i = inst(SshAuth::Password, None, 22);
        i.ssh.as_mut().unwrap().password = Some("s3cr3t".into());
        let cmd = command(&i, &[], None).unwrap();
        let std = cmd.as_std();
        let argv: Vec<String> = std
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !argv.iter().any(|a| a.contains("s3cr3t")),
            "密码不得进 argv: {argv:?}"
        );
        assert!(
            std.get_envs()
                .any(|(k, v)| k == "SSHPASS" && v == Some(std::ffi::OsStr::new("s3cr3t"))),
            "密码应经 SSHPASS 环境变量传递"
        );
    }
}
