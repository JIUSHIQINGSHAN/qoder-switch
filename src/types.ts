// 与 Rust 侧 serde 形状一一对应。枚举是外部标签：unit variant 序列化成裸字符串，
// newtype variant 序列化成 { Variant: 值 } —— hosted 字段的三种形状由此而来。

export type QoderVariant = "cn" | "global";
export type QoderTarget = "desktop" | "cli" | "work";
export type Hosted = "No" | { Yes: string } | { Unknown: string };

export type FileRole =
  | "auth_main"
  | "auth_v2"
  | "profile_overlays"
  | "desktop_machine_id"
  | "local_state"
  | "channel_activation"
  | "cli_user"
  | "cli_machine_id"
  | "status_echo";

export interface CredentialView {
  role: string;
  path: string;
  critical: boolean;
  exists: boolean;
  size: number | null;
}

export interface AxisStatus {
  variant: QoderVariant;
  target: QoderTarget;
  variant_label: string;
  target_label: string;
  images: string[];
  running_pids: number[];
  hosted: Hosted;
  exe: string | null;
  credentials: CredentialView[];
}

export interface Identity {
  name?: string | null;
  email?: string | null;
  plan?: string | null;
  product?: string | null;
  logged_in?: boolean | null;
  snapshot_at?: string | null;
  // 下面三项只有解密登录态成功时才有。
  uid?: string | null;
  expires_at?: string | null;
  refresh_expires_at?: string | null;
}

export interface Member {
  role: FileRole;
  file_name: string;
  sha256: string;
  size: number;
  critical: boolean;
}

export interface Bundle {
  account_id: string;
  variant: QoderVariant;
  target: QoderTarget;
  created_at: string;
  members: Member[];
  identity: Identity;
}

export interface Preview {
  req: { account_id: string; variant: QoderVariant; target: QoderTarget; restart: boolean };
  layout: [string, string, boolean][];
  writes: string[];
  running_pids: number[];
  hosted: Hosted;
  warnings: string[];
}

export type Phase = "Prepared" | "TargetClosed" | "Completed" | "RolledBack" | "Failed";

export interface Journal {
  id: string;
  account_id: string;
  variant: QoderVariant;
  target: QoderTarget;
  started_at: string;
  phase: Phase;
  backup_dir: string;
  note?: string | null;
}

export interface Change {
  kind: "Appeared" | "Disappeared" | "Modified" | "TouchOnly" | "Unchanged";
  variant: QoderVariant;
  target: QoderTarget;
  role: string;
  path: string;
  critical: boolean;
}

export interface SnapshotReport {
  taken_at: string;
  path: string;
  changes: Change[];
}
