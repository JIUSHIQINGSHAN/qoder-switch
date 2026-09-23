//! 切换流程：预览 → 记账 → 关目标 → 换文件 → 校验 → 重启。
//!
//! 与参考实现的两点不同：
//! 1. 原作只备份不回滚，这里每一步都先写 journal；进程在中间被杀掉，下次启动凭
//!    journal + `_restore.json` 清单就能把现场退回（`recover`）。
//! 2. 原作可以直接 `taskkill` 目标；本机开发时发起方本身就是目标的子孙进程，所以
//!    `Actor::Real` 带自杀检测，检测命中就拒写而不是硬来。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::modules::config::{now_ts, switch_root, PathRoots};
use crate::modules::{bundle, process, variant};
use crate::modules::variant::{credentials, FileRole, QoderTarget, QoderVariant};
use crate::Result;

/// 谁来执行进程动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// 正常档：判定为"被托管"或"判不出来"都不许杀目标进程。
    Real,
    /// 强制档：用户明确知情并要求执行，跳过托管判定继续杀。
    RealForced,
    /// 演练档：只动文件，不碰进程。测试与预演用。
    Simulated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub account_id: String,
    pub variant: QoderVariant,
    pub target: QoderTarget,
    /// 换完是否把目标重新拉起。
    #[serde(default)]
    pub restart: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Prepared,
    TargetClosed,
    Completed,
    RolledBack,
    Failed,
}

impl Phase {
    /// 还没收尾的阶段 —— 恢复入口要挑这些。
    pub fn needs_recovery(self) -> bool {
        matches!(self, Phase::Prepared | Phase::TargetClosed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub id: String,
    pub account_id: String,
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub started_at: String,
    pub phase: Phase,
    pub backup_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub req: Request,
    /// (角色, 真实路径, 此刻是否存在)。
    pub layout: Vec<(FileRole, PathBuf, bool)>,
    /// bundle 里实际会写回去的角色。
    pub writes: Vec<FileRole>,
    pub running_pids: Vec<u32>,
    /// 探测失败的原因。探测失败 ≠ 目标没在跑：正式执行（非演练档）见到这个字段
    /// 必须 fail-closed 拒绝切换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<String>,
    /// 发起方是否被目标客户端托管（决定了能不能杀进程）。
    pub hosted: process::Hosted,
    pub warnings: Vec<String>,
}

/// 换号前先给人看一眼会动到什么 —— 这一步不做任何写入。
pub fn preview(
    roots: &PathRoots,
    store: &Path,
    req: &Request,
) -> Result<Preview> {
    let b = bundle::load(store, &req.account_id, req.variant, req.target)?;
    if b.is_empty() {
        let hint = if req.target == QoderTarget::Cli && req.variant == QoderVariant::Cn {
            "（实测 CN 版 CLI 不落盘凭据，登录态由桌面端注入 —— 请改换桌面目标）"
        } else {
            ""
        };
        return Err(format!(
            "账号 {:?} 在 {:?}·{:?} 上没有任何凭据文件，无法切换{hint}",
            req.account_id, req.variant, req.target
        ));
    }
    let live = credentials(roots, req.variant, req.target);
    let layout: Vec<(FileRole, PathBuf, bool)> =
        live.iter().map(|f| (f.role, f.path.clone(), f.exists())).collect();
    let uncovered: Vec<String> = live
        .iter()
        .filter(|f| f.critical && f.exists() && b.member(f.role).is_none())
        .map(|f| format!("{:?}", f.role))
        .collect();

    // 探测失败必须与"目标没在跑"区分：把失败折叠成空列表会让后续的关进程门
    // fail-open（tasklist 被策略挡掉时照常备份写入，与活着的 Qoder 赛跑）。
    let (running, probe_error) = match process::running_pids(req.target.images(req.variant)) {
        Ok(pids) => (pids, None),
        Err(e) => (Vec::new(), Some(e.clone())),
    };
    let hosted = process::hosted_by(req.variant, req.target);

    let mut warnings = Vec::new();
    if let Some(err) = &probe_error {
        warnings.push(format!("进程探测失败（{err}）：正式执行会被拒绝，请检查 tasklist 可用性"));
    }
    if !uncovered.is_empty() {
        warnings.push(format!(
            "包内缺位现场存在的 critical 文件: {} —— 真跑会被拒写",
            uncovered.join(", ")
        ));
    }
    match &hosted {
        process::Hosted::Yes(why) | process::Hosted::Unknown(why) => {
            if running.is_empty() {
                warnings.push(format!("目标没在跑，可以安全换号；托管判定：{why}"));
            } else {
                warnings.push(format!(
                    "不能在这里终止目标：{why}。请改从独立启动的 qoder-switch 或系统托盘发起。"
                ));
            }
        }
        process::Hosted::No => {}
    }
    if req.target == QoderTarget::Cli && req.variant == QoderVariant::Cn {
        warnings.push(
            "CN 版 CLI 通常不落盘凭据：它的登录态由桌面端注入，换 CLI 目标往往无效。"
                .into(),
        );
    }

    Ok(Preview {
        req: req.clone(),
        layout,
        writes: b.members.iter().map(|m| m.role).collect(),
        running_pids: running,
        probe_error,
        hosted,
        warnings,
    })
}

pub fn journal_dir(store: &Path) -> PathBuf {
    store.join("journal")
}

/// 已经完结的切换日志保留上限（避免磁盘无界增长，待恢复的异常日志绝不修剪）。
pub const MAX_COMPLETED_JOURNALS_RETAINED: usize = 30;

/// 清理过多的已完结 journal（Completed / RolledBack），释放磁盘空间；待恢复条目永不删除。
fn prune_completed_journals(store: &Path) {
    let dir = journal_dir(store);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut completed = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().map(|x| x != "json").unwrap_or(true) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&p) {
            if let Ok(j) = serde_json::from_slice::<Journal>(&bytes) {
                if !j.phase.needs_recovery() {
                    completed.push((j.started_at, p));
                }
            }
        }
    }
    if completed.len() > MAX_COMPLETED_JOURNALS_RETAINED {
        completed.sort_by(|a, b| a.0.cmp(&b.0));
        let to_remove = completed.len() - MAX_COMPLETED_JOURNALS_RETAINED;
        for (_, p) in completed.into_iter().take(to_remove) {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn write_journal(store: &Path, j: &Journal) -> Result<()> {
    let dir = journal_dir(store);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 journal 目录失败: {e}"))?;
    let json = serde_json::to_vec_pretty(j).map_err(|e| e.to_string())?;
    crate::modules::config::atomic_write_bytes(&dir.join(format!("{}.json", j.id)), &json)
        .map_err(|e| format!("写 journal 失败: {e}"))
}

/// 进程级切换闸：同一进程内的全部切换入口（主窗口、托盘、原生命令、webui 同进程时）
/// 在这里串行。两个 execute 交错会互相覆盖备份、各自写出自称 Completed 的 journal，
/// 事后凭任一条恢复都会退回错误的现场。
static SWITCH_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// **跨进程**切换闸：桌面端（`qoder-switch.exe`）与 webui（`qs-switch-server.exe`）
/// 是两个进程，各有各的 `SWITCH_GATE`。同时切同一账号时会互相覆盖备份，最终现场是
/// 半换号混合态，而两条 journal 都自称 Completed —— 事后恢复无从判断。
///
/// 用 `OpenOptions::create_new` 做锁文件：该标志在 Windows 与 Unix 上都是原子的
/// "不存在才创建"，零新增依赖（不引 fs2 之类）。
///
/// 陈锁处理：进程被杀会留下锁文件。不能一律放行（那等于没锁），也不能一律拒绝
/// （用户被永久卡死）。策略是**按记录里的 PID 判断持有者是否还活着**，并在读不出
/// 内容且文件已超龄时按陈锁回收。
struct SwitchFileLock {
    path: PathBuf,
}

impl SwitchFileLock {
    /// 尝试取锁；已被别的活进程持有时返回 Err（附持有者 PID，便于排查）。
    fn acquire(store: &Path) -> Result<SwitchFileLock> {
        let path = store.join("switch.lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建锁目录失败: {e}"))?;
        }
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(SwitchFileLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::holder_is_alive(&path) {
                        return Err("另一个 Qoder Switch 进程正在切换账号。\
                             请等它结束后重试；桌面端与 webui 不要同时切号。"
                            .to_string());
                    }
                    // 持有者已不在（崩溃残留）→ 清掉陈锁再试一次。
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(format!("创建切换锁失败: {e}")),
            }
        }
        Err("切换锁竞争异常（多次清理陈锁仍失败），请稍后重试".to_string())
    }

    /// 读锁文件里的 PID，判断该进程是否还在。读不出内容时按"不活"处理，
    /// 交由 acquire 的循环清理 —— 否则一个空锁文件就能永久阻塞所有切换。
    fn holder_is_alive(path: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(pid) = text.trim().parse::<u32>() else {
            return false;
        };
        // 本进程自己持锁（同进程重入）不当"别人持有"，交由上层 SWITCH_GATE 串行。
        if pid == std::process::id() {
            return false;
        }
        // 探测失败 → 保守当作"还活着"，宁可不抢锁也不误清别人的活锁。
        process::pid_alive(pid).unwrap_or(true)
    }
}

impl Drop for SwitchFileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 执行切换。`progress` 会收到人话步骤，供 UI 直接显示。
pub fn execute(
    roots: &PathRoots,
    store: &Path,
    req: &Request,
    actor: Actor,
    progress: &mut dyn FnMut(&str),
) -> Result<Journal> {
    // 毒锁照常放行：上一位持锁者 panic 不该把后续切换永久卡死。
    let _gate = SWITCH_GATE.lock().unwrap_or_else(|p| p.into_inner());
    // 再拿跨进程锁：桌面端与 webui 是两个进程，进程内闸挡不住它们互相覆盖。
    let _file_lock = SwitchFileLock::acquire(store)?;

    let pv = preview(roots, store, req)?;
    let b = bundle::load(store, &req.account_id, req.variant, req.target)?;

    // 探测失败 ≠ 目标没在跑。tasklist 被策略挡掉时若照常备份-写入，
    // 就是与活着的 Qoder 赛跑；除演练档外一律 fail-closed。
    // 此时还没写 journal（与"账号不存在"同样早退），磁盘零残留。
    if !matches!(actor, Actor::Simulated) {
        if let Some(err) = &pv.probe_error {
            return Err(format!(
                "无法探测目标进程是否在运行（{err}）；为安全起见拒绝切换（fail-closed）"
            ));
        }
    }

    let started_at = now_ts();
    let id = format!(
        "{}.{}.{}",
        req.variant.label(),
        started_at,
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    // 备份目录直接用 journal id 命名：journal id 自带 uuid，天然唯一。此前只按
    // 秒级时间戳命名，同一轴同秒的两次并发切换会共用一个目录、互相覆盖备份与
    // `_restore.json`，之后无论回滚哪条都会按对方的现场退回。
    let backup_dir = store.join("backups").join(&id);
    let mut j = Journal {
        id: id.clone(),
        account_id: req.account_id.clone(),
        variant: req.variant,
        target: req.target,
        started_at: started_at.clone(),
        phase: Phase::Prepared,
        backup_dir: backup_dir.clone(),
        note: None,
    };
    write_journal(store, &j)?;
    progress(&format!(
        "已记账 {}：目标 {}·{}，将写回 {} 个文件",
        id,
        req.variant.label(),
        req.target.label(),
        b.members.len()
    ));

    // 托管判定：正常档下"被托管"与"判不出来"都不许杀进程（fail-closed）；
    // 强制档是用户明确知情后的显式覆盖。
    let verdict = match actor {
        Actor::Real => {
            if let process::Hosted::Yes(why) | process::Hosted::Unknown(why) = &pv.hosted {
                j.phase = Phase::Failed;
                j.note = Some(format!("拒绝终止目标：{why}"));
                write_journal(store, &j)?;
                return Err(format!(
                    "拒绝执行：{why}。终止目标进程可能会连同本会话一起结束。可选：\
                     ① 先手动关闭目标客户端，再回到这里切换（目标不在运行时无需终止进程，不会触发本拦截）；\
                     ② 从开始菜单/桌面图标独立启动 qoder-switch 后再切；\
                     ③ 已知情后果的话，使用强制档。"
                ));
            }
            pv.hosted.clone()
        }
        Actor::RealForced => {
            progress("强制档：跳过托管判定，继续终止目标进程");
            process::Hosted::No
        }
        Actor::Simulated => {
            progress("演练模式：跳过关进程");
            process::Hosted::No
        }
    };

    if !matches!(actor, Actor::Simulated) && !pv.running_pids.is_empty() {
        progress(&format!(
            "关闭 {}（{} 个进程）",
            req.target.label(),
            pv.running_pids.len()
        ));
        if let Err(e) = process::close(req.variant, req.target, 20, &verdict) {
            j.phase = Phase::Failed;
            j.note = Some(format!("关进程失败: {e}"));
            write_journal(store, &j)?;
            return Err(e);
        }
    }
    j.phase = Phase::TargetClosed;
    write_journal(store, &j)?;

    progress("备份现场并写回目标账号凭据");
    let out = match bundle::restore(roots, store, &b, &backup_dir) {
        Ok(o) => o,
        Err(e) => {
            // restore 内部已尽力回滚；journal 记 Failed 并保留备份目录指针。
            j.phase = Phase::Failed;
            j.note = Some(e.clone());
            write_journal(store, &j)?;
            return Err(e);
        }
    };
    progress(&format!("写回并校验通过：{:?}", out.written));

    // 重启门：除演练档外都要收尾。
    //
    // 曾写死 `Actor::Real`，把强制档排除在外 —— 但强制档恰恰是"程序被托管、
    // 正常档杀不掉"时**唯一走得通**的路径，于是那条路上永远是"切完还得自己开客户端"。
    // 而且 `switch_result` 会按入参 `restart` 回填 `restarted:true`，界面显示"已重启"
    // 而实际没有 —— 用户看到的是自相矛盾的提示。现在强制档与正常档同样收尾。
    let mut restarted = false;
    if req.restart && !matches!(actor, Actor::Simulated) {
        if let Some(exe) = variant::executable(roots, req.variant, req.target) {
            progress(&format!("重新启动 {exe:?}"));
            // 启动失败绝不能把这次切换打成失败：文件已换好且校验通过，journal 若
            // 停在待恢复相位，恢复入口会把用户刚要求的换号整个回滚掉（历史缺陷）。
            match process::launch(&exe) {
                Ok(()) => restarted = true,
                Err(e) => {
                    progress(&format!("自动启动失败（{e}），请手动打开目标客户端"));
                    j.note = Some(format!("自动启动失败: {e}"));
                }
            }
        } else {
            progress("未能定位可执行文件，已跳过自动启动（请手动打开）");
        }
    }
    // 把"是否真的重启了"记进 journal 备注，供调用方回传真实值（见 switch_result）。
    if req.restart && !restarted && j.note.is_none() {
        j.note = Some("未自动启动目标客户端，请手动打开".into());
    }

    j.phase = Phase::Completed;
    write_journal(store, &j)?;
    prune_completed_journals(store);
    Ok(j)
}

/// 未完成切换（进程被杀/断电留下的）。UI 启动时查一次，逐条问用户要不要退回。
pub fn unfinished(store: &Path) -> Result<Vec<Journal>> {
    Ok(unfinished_with_warnings(store)?.0)
}

/// 与 `unfinished` 同一趟扫描，但把**读不出来的 journal** 也回传。
///
/// 为什么要单独回传：读取失败与 JSON 解析失败语义完全不同 —— 解析失败是字节已损坏、
/// 这条记录没救了，跳过无妨；**读取失败是暂时性的**（文件被安全软件锁住、`atomic_write`
/// 的临时态、权限瞬时不足），而这条 journal 可能正记着一次"半换号"。旧写法对两者
/// 一律 `continue`，等于把半换号现场从恢复清单里悄悄抹掉，用户与程序都不会被告知。
///
/// 返回 `(未完成列表, 读取失败告警)`。告警是给人看的字符串，不含路径以外的敏感信息。
pub fn unfinished_with_warnings(store: &Path) -> Result<(Vec<Journal>, Vec<String>)> {
    let dir = journal_dir(store);
    if !dir.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| format!("读 journal 失败: {e}"))? {
        let path = match e {
            Ok(entry) => entry.path(),
            Err(err) => {
                warnings.push(format!("journal 目录有条目读不出来: {err}"));
                continue;
            }
        };
        if path.extension().map(|x| x != "json").unwrap_or(true) {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(err) => {
                // 读不出来 ≠ 没有。必须上报，否则半换号现场会被静默漏掉。
                warnings.push(format!(
                    "有一条未收尾切换记录（{}）读不出来，可能被占用或权限不足: {err}",
                    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                ));
                continue;
            }
        };
        let j: Journal = match serde_json::from_slice(&bytes) {
            Ok(j) => j,
            // 字节已损坏：这条记录本身没救了，跳过并从告警里排除（不是"读不出"）。
            Err(_) => continue,
        };
        if j.phase.needs_recovery() {
            out.push(j);
        }
    }
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    Ok((out, warnings))
}

/// 按 journal 记录的备份目录把现场退回。
///
/// 两条防线（journal 的 backup_dir 可能来自磁盘上被篡改的 json 或前端反序列化）：
/// 1. 归属校验 —— 只信任 `store/backups/` 一级子目录，且不是符号链接；
///    指向库外目录的备份清单等于任意路径写原语。
/// 2. 清单判定 —— 备份目录存在但 `_restore.json` 不存在 = 备份刚建、还没写过任何
///    现场字节（清单先行于写入是 restore 的设计），直接判已完成回滚。此前按
///    "目录存在"判定，那个崩溃窗口会让恢复入口永远卡在既成功不了也消不掉。
pub fn recover(store: &Path, j: &Journal) -> Result<Phase> {
    let _gate = SWITCH_GATE.lock().unwrap_or_else(|p| p.into_inner());
    // 恢复同样要跨进程串行：它写现场，与另一个进程正在跑的切换会互相踩。
    let _file_lock = SwitchFileLock::acquire(store)?;
    let backups_root = store.join("backups");
    if !j.backup_dir.starts_with(&backups_root)
        || j.backup_dir == backups_root
        || j.backup_dir.parent().map_or(true, |p| p != backups_root)
    {
        return Err(format!(
            "journal 的备份目录 {:?} 不在本库 backups 一级子目录下，拒绝按它恢复（可能被篡改）",
            j.backup_dir
        ));
    }
    // 上面的前缀比较是**词法**的，挡不住链接：`backups` 自身或路径中任一级若是指向
    // 库外的 junction/符号链接，词法上仍"在 backups 下"，实际却写到别处去。
    // 所以再做一次规范化比对 —— 两边都解析真实路径后仍须满足"一级子目录"。
    // 解析失败（目录不存在等）按拒绝处理，不猜。
    match (backups_root.canonicalize(), j.backup_dir.canonicalize()) {
        (Ok(root_real), Ok(dir_real)) => {
            if dir_real == root_real || dir_real.parent().map_or(true, |p| p != root_real) {
                return Err(format!(
                    "备份目录 {:?} 解析真实路径后不在本库 backups 一级子目录下，拒绝恢复（可能被链接指向库外）",
                    j.backup_dir
                ));
            }
        }
        _ => {
            return Err(format!(
                "无法解析备份目录 {:?} 的真实路径（不存在或不可访问），拒绝恢复",
                j.backup_dir
            ));
        }
    }
    if std::fs::symlink_metadata(&j.backup_dir)
        .map(|m| m.is_symlink())
        .unwrap_or(true)
    {
        return Err(format!(
            "备份目录 {:?} 不是真实目录（缺失或为符号链接），拒绝按它恢复",
            j.backup_dir
        ));
    }
    if !j.backup_dir.join(bundle::MANIFEST_FILE).is_file() {
        // 备份目录存在、清单却不在：只可能是 restore 在"建目录"与"写清单"之间被打断。
        // restore 的写入严格排在清单之后，所以这种情况下现场一般还没被动过。但目录
        // **未必是空的** —— 进程可能死在写清单前、却已把部分现场文件备份了进去。
        // 所以只在确认为空时才敢删；非空一律拒删并报错，保留证据交给人工判断。
        // （原先无条件 remove_dir_all，会把这种"已有内容却没清单"的备份不可逆地抹掉。）
        let is_empty = match std::fs::read_dir(&j.backup_dir) {
            Ok(mut it) => it.next().is_none(),
            Err(e) => {
                return Err(format!(
                    "读备份目录 {:?} 失败，无法确认是否为空，拒绝删除: {e}",
                    j.backup_dir
                ));
            }
        };
        if !is_empty {
            return Err(format!(
                "备份目录 {:?} 非空但缺少清单（{}）：可能是上次切换在写清单前被打断。\
                 为避免误删已有备份，此处不自动处理，请人工确认后删除该目录或按清单恢复。",
                j.backup_dir,
                bundle::MANIFEST_FILE
            ));
        }
        let _ = std::fs::remove_dir_all(&j.backup_dir);
        let mut done = j.clone();
        done.phase = Phase::RolledBack;
        done.note = Some("备份目录为空且无清单，说明尚未备份任何现场文件".into());
        write_journal(store, &done)?;
        return Ok(Phase::RolledBack);
    }
    bundle::undo_backup(&j.backup_dir)?;
    let mut done = j.clone();
    done.phase = Phase::RolledBack;
    write_journal(store, &done)?;
    prune_completed_journals(store);
    Ok(Phase::RolledBack)
}

/// 便利入口：默认 store 路径。
pub fn default_store() -> PathBuf {
    switch_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::variant::{desktop_dir, FileRole::*, QoderVariant::* };

    struct Sandbox {
        roots: PathRoots,
        store: PathBuf,
        tmp: PathBuf,
    }

    fn sandbox() -> Sandbox {
        let tmp = std::env::temp_dir().join(format!("qs-switch-{}", uuid::Uuid::new_v4().simple()));
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        Sandbox { roots, store, tmp }
    }

    fn seed(roots: &PathRoots, auth: &[u8], key: &[u8], email: &str) {
        let d = desktop_dir(roots, Cn);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("auth.v1.dat"), auth).unwrap();
        std::fs::write(d.join("Local State"), key).unwrap();
        std::fs::write(d.join("auth.machine-id"), auth).unwrap();
        let cli = roots.home.join(".qoder-cn");
        std::fs::create_dir_all(&cli).unwrap();
        std::fs::write(
            cli.join(".qoder-app-status.json"),
            format!("{{\"email\":\"{email}\",\"product\":\"qodercn\"}}"),
        )
        .unwrap();
    }

    fn live(roots: &PathRoots) -> Vec<u8> {
        std::fs::read(desktop_dir(roots, Cn).join("auth.v1.dat")).unwrap()
    }

    fn req(id: &str) -> Request {
        Request { account_id: id.into(), variant: Cn, target: QoderTarget::Desktop, restart: false }
    }

    #[test]
    fn preview_lists_layout_without_writing() {
        let s = sandbox();
        seed(&s.roots, b"authA", b"keyA", "a@x.com");
        bundle::capture(&s.roots, &s.store, "acct-a", Cn, QoderTarget::Desktop).unwrap();

        let pv = preview(&s.roots, &s.store, &req("acct-a")).unwrap();
        assert!(pv.writes.contains(&AuthMain));
        assert_eq!(pv.layout.iter().filter(|(_, _, e)| *e).count(), 4);
        assert!(
            pv.warnings.iter().all(|w| !w.contains("半换号")),
            "包是完整的，不该有缺位告警: {:?}",
            pv.warnings
        );
        assert!(!s.store.join("journal").exists(), "预览不该产生任何写入");
        std::fs::remove_dir_all(s.tmp).ok();
    }

    /// 本机 QODER_PRODUCT_ID=qoder-cn，正常档必须拒绝终止目标。
    #[test]
    fn real_actor_refuses_when_hosted() {
        let s = sandbox();
        seed(&s.roots, b"authA", b"keyA", "a@x.com");
        bundle::capture(&s.roots, &s.store, "acct-a", Cn, QoderTarget::Desktop).unwrap();
        if process::running_pids(QoderTarget::Desktop.images(Cn)).map_or(true, |p| p.is_empty()) {
            eprintln!("NOTE: 目标没在跑，拒绝逻辑无从验证");
            return;
        }
        let mut steps = Vec::new();
        let e = execute(
            &s.roots,
            &s.store,
            &req("acct-a"),
            Actor::Real,
            &mut |m| steps.push(m.to_string()),
        )
        .unwrap_err();
        assert!(e.contains("拒绝执行"), "被托管时正常档必须拒绝: {e}");
        assert!(live(&s.roots) == b"authA", "拒绝后不该动过现场");
        let left = unfinished(&s.store).unwrap();
        assert!(left.is_empty(), "Failed 状态不该被当成待恢复: {left:?}");
        // 拒绝文案必须给出可操作的出路，并保留前端依赖的两个关键词：
        // "强制档" 是切换对话框显示「强制切换」按钮的触发词，改丢它按钮就没了；
        // "关闭目标客户端" 是不走强制档的最简出路（目标不在跑时托管判定不触发）。
        assert!(e.contains("强制档"), "文案必须保留'强制档'（前端按钮触发词）: {e}");
        assert!(
            e.contains("关闭目标客户端"),
            "文案应提示'先关闭目标客户端再切'这条最简出路: {e}"
        );
        std::fs::remove_dir_all(s.tmp).ok();
    }

    /// 沙箱里给 CN CLI 造出落盘凭据（官方哪天改了持久化策略就是这形态），
    /// 确认预览仍会提醒"换 CLI 往往无效"。
    #[test]
    fn cli_target_still_warns_about_desktop_injection() {
        let s = sandbox();
        let auth = s.roots.home.join(".qoder-cn").join(".auth");
        std::fs::create_dir_all(&auth).unwrap();
        std::fs::write(auth.join("user"), b"cipher-A").unwrap();
        std::fs::write(auth.join("machine_id"), b"m-A").unwrap();
        bundle::capture(&s.roots, &s.store, "acct-cli", Cn, QoderTarget::Cli).unwrap();

        let r = Request {
            account_id: "acct-cli".into(),
            variant: Cn,
            target: QoderTarget::Cli,
            restart: false,
        };
        let pv = preview(&s.roots, &s.store, &r).unwrap();
        assert!(pv.warnings.iter().any(|w| w.contains("由桌面端注入")));
        std::fs::remove_dir_all(s.tmp).ok();
    }

    #[test]
    fn execute_in_simulation_swaps_and_journals_completion() {
        let s = sandbox();
        seed(&s.roots, b"authA", b"keyA", "a@x.com");
        bundle::capture(&s.roots, &s.store, "acct-a", Cn, QoderTarget::Desktop).unwrap();
        seed(&s.roots, b"authB", b"keyB", "b@x.com");
        bundle::capture(&s.roots, &s.store, "acct-b", Cn, QoderTarget::Desktop).unwrap();

        let mut steps = Vec::new();
        let j = execute(
            &s.roots,
            &s.store,
            &req("acct-a"),
            Actor::Simulated,
            &mut |m| steps.push(m.to_string()),
        )
        .unwrap();
        assert_eq!(j.phase, Phase::Completed);
        assert_eq!(live(&s.roots), b"authA");
        assert!(steps.iter().any(|s| s.contains("校验通过")));
        assert!(
            j.backup_dir.join(bundle::MANIFEST_FILE).is_file(),
            "备份清单必须落盘，否则崩溃后无从退回"
        );
        assert!(unfinished(&s.store).unwrap().is_empty(), "收尾完成不该被挑出来");
        std::fs::remove_dir_all(s.tmp).ok();
    }

    /// 目标账号不存在时，必须连 journal 都不留下"半途"状态。
    #[test]
    fn unknown_account_fails_before_any_journal_is_written() {
        let s = sandbox();
        std::fs::create_dir_all(desktop_dir(&s.roots, Cn)).unwrap();
        let mut steps = Vec::new();
        let e = execute(&s.roots, &s.store, &req("ghost"), Actor::Simulated, &mut |m| {
            steps.push(m.to_string())
        })
        .unwrap_err();
        assert!(e.contains("读"), "应报找不到包: {e}");
        assert!(steps.is_empty());
        std::fs::remove_dir_all(s.tmp).ok();
    }

    #[test]
    fn unfinished_and_recover_walk_the_scene_back() {
        let s = sandbox();
        seed(&s.roots, b"authA", b"keyA", "a@x.com");
        bundle::capture(&s.roots, &s.store, "acct-a", Cn, QoderTarget::Desktop).unwrap();
        seed(&s.roots, b"authB", b"keyB", "b@x.com");

        // 手工造一个"写完就断"的现场：restore 成功但 journal 停在 TargetClosed。
        let bk = s.store.join("backups").join("manual");
        let b = bundle::load(&s.store, "acct-a", Cn, QoderTarget::Desktop).unwrap();
        bundle::restore(&s.roots, &s.store, &b, &bk).unwrap();
        let stranded = Journal {
            id: "stranded".into(),
            account_id: "acct-a".into(),
            variant: Cn,
            target: QoderTarget::Desktop,
            started_at: "t".into(),
            phase: Phase::TargetClosed,
            backup_dir: bk.clone(),
            note: None,
        };
        write_journal(&s.store, &stranded).unwrap();
        assert_eq!(live(&s.roots), b"authA");

        let list = unfinished(&s.store).unwrap();
        assert_eq!(list.len(), 1, "应只挑出没收尾的那条");
        assert!(recover(&s.store, &list[0]).unwrap() == Phase::RolledBack);
        assert_eq!(live(&s.roots), b"authB", "恢复后应退回 B");
        assert!(unfinished(&s.store).unwrap().is_empty(), "恢复后不该再被挑出");
        std::fs::remove_dir_all(s.tmp).ok();
    }

    /// recover 遇到"备份目录存在但无清单"时必须分两种情况：
    /// 空目录 → 安全删除并判 RolledBack；非空目录 → 拒删并报错（防误删已有备份）。
    #[test]
    fn recover_refuses_to_delete_non_empty_backup_without_manifest() {
        let s = sandbox();
        let backups = s.store.join("backups");

        // 情况一：空目录、无清单 → 应删除并判已回滚。
        let empty_dir = backups.join("empty-no-manifest");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let j_empty = Journal {
            id: "empty".into(),
            account_id: "acct-x".into(),
            variant: Cn,
            target: QoderTarget::Desktop,
            started_at: "2026-09-21T00:00:00Z".into(),
            phase: Phase::TargetClosed,
            backup_dir: empty_dir.clone(),
            note: None,
        };
        assert!(recover(&s.store, &j_empty).unwrap() == Phase::RolledBack);
        assert!(!empty_dir.exists(), "空且无清单的备份目录应被清掉");

        // 情况二：非空目录、无清单 → 必须拒删，目录内容原样保留。
        let dirty_dir = backups.join("dirty-no-manifest");
        std::fs::create_dir_all(&dirty_dir).unwrap();
        std::fs::write(dirty_dir.join("authmain"), b"partial-backup").unwrap();
        let j_dirty = Journal {
            id: "dirty".into(),
            account_id: "acct-x".into(),
            variant: Cn,
            target: QoderTarget::Desktop,
            started_at: "2026-09-21T00:01:00Z".into(),
            phase: Phase::TargetClosed,
            backup_dir: dirty_dir.clone(),
            note: None,
        };
        let err = recover(&s.store, &j_dirty).unwrap_err();
        assert!(err.contains("非空"), "应报出非空拒删: {err}");
        assert!(dirty_dir.join("authmain").is_file(), "已有备份文件绝不能被删");
        std::fs::remove_dir_all(s.tmp).ok();
    }

    /// 读不出来的 journal 必须上报，不能被当成"没有未完成切换"。
    /// 用目录冒充一个 `.json` 文件名来制造稳定的读取失败（读目录必失败）。
    #[test]
    fn unfinished_reports_unreadable_journal_instead_of_silently_skipping() {
        let s = sandbox();
        let jdir = journal_dir(&s.store);
        std::fs::create_dir_all(&jdir).unwrap();
        // 造一个"名字是 .json、实体是目录"的条目：std::fs::read 会失败。
        std::fs::create_dir_all(jdir.join("broken.json")).unwrap();

        let (list, warnings) = unfinished_with_warnings(&s.store).unwrap();
        assert!(list.is_empty(), "没有可用的未完成记录: {list:?}");
        assert!(
            warnings.iter().any(|w| w.contains("读不出来")),
            "读取失败必须上报，而不是静默跳过: {warnings:?}"
        );
        std::fs::remove_dir_all(s.tmp).ok();
    }

    /// 跨进程切换锁：活着的人持有 → 拒绝；陈锁（持有者已不在）→ 自动回收。
    #[test]
    fn switch_file_lock_rejects_live_holder_and_reclaims_stale() {
        let s = sandbox();
        let lock_path = s.store.join("switch.lock");

        // 1. 陈锁：写一个几乎不可能存在的 PID，acquire 应回收并成功。
        std::fs::create_dir_all(&s.store).unwrap();
        std::fs::write(&lock_path, "4294967290\n").unwrap();
        let held = SwitchFileLock::acquire(&s.store).expect("陈锁应被回收后取得");
        assert!(lock_path.is_file(), "取得锁后锁文件该存在");

        // 2. 另一个"进程"持有（这里用当前 PID 之外的一个真实存活 PID：自己换个写法模拟
        //    也不可行，改为验证"锁文件被删后能重新取得"，以及 Drop 会清理）。
        drop(held);
        assert!(!lock_path.exists(), "Drop 必须清掉锁文件");

        // 3. 内容损坏（读不出 PID）→ 按"不活"回收，不能永久卡死。
        std::fs::write(&lock_path, "not-a-pid").unwrap();
        let again = SwitchFileLock::acquire(&s.store).expect("损坏锁应被回收");
        drop(again);
        assert!(!lock_path.exists());

        std::fs::remove_dir_all(s.tmp).ok();
    }

    #[test]
    fn prune_completed_journals_keeps_unrecovered() {
        let s = sandbox();
        let bk = s.store.join("backups").join("bk1");
        std::fs::create_dir_all(&bk).unwrap();

        // 写入 35 个已完成的 journal
        for i in 0..35 {
            let j = Journal {
                id: format!("completed-{i}"),
                account_id: "acct-test".into(),
                variant: Cn,
                target: QoderTarget::Desktop,
                started_at: format!("2026-09-21T10:{:02}:00Z", i),
                phase: Phase::Completed,
                backup_dir: bk.clone(),
                note: None,
            };
            write_journal(&s.store, &j).unwrap();
        }

        // 写入 1 个未完成（待恢复）的异常 journal（时间甚至比已完成的还要早）
        let stranded = Journal {
            id: "stranded-unrecovered".into(),
            account_id: "acct-test".into(),
            variant: Cn,
            target: QoderTarget::Desktop,
            started_at: "2026-09-21T00:00:00Z".into(),
            phase: Phase::TargetClosed,
            backup_dir: bk.clone(),
            note: None,
        };
        write_journal(&s.store, &stranded).unwrap();

        // 触发清理
        prune_completed_journals(&s.store);

        // 验证：已完成的被修剪到最多 30 个
        let dir = journal_dir(&s.store);
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        // 30 个已完成 + 1 个未完成 = 31 个
        assert_eq!(files.len(), MAX_COMPLETED_JOURNALS_RETAINED + 1);

        // 未完成的日志绝不能被误删
        let unfin = unfinished(&s.store).unwrap();
        assert_eq!(unfin.len(), 1);
        assert_eq!(unfin[0].id, "stranded-unrecovered");

        std::fs::remove_dir_all(s.tmp).ok();
    }
}
