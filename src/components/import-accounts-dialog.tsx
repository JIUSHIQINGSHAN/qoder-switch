import { useEffect, useRef, useState } from "react";
import { FileUp, Loader2 } from "lucide-react";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import * as api from "@/lib/api";
import { DEFAULT_VARIANT, variantLabel } from "@/lib/variant";
import type { ImportPreviewAccount, WbVariant } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 导入完成后回调（参数为导入结果计数）。 */
  onImported?: (result: { imported: number; skipped: number; overwritten: number }) => void;
  /** 当前档位；仅用于文案，导入结果按文件内各账号自身的档位归类。 */
  variant?: WbVariant;
}

/** 导入预览账号展示名（脱敏展示：昵称/邮箱/uid）。 */
function previewLabel(a: ImportPreviewAccount): string {
  return a.nickname || a.email || a.uid || `第 ${a.index + 1} 项`;
}

/** 导入账号弹框：选 JSON 文件 → 后端解析预览 → 勾选账号 → 导入合并。 */
export function ImportAccountsDialog({
  open,
  onOpenChange,
  onImported,
  variant = DEFAULT_VARIANT,
}: Props) {
  const fileInputRef = useRef<HTMLInputElement>(null);
  // 预览请求的过期令牌：慢的旧文件响应回来时不得覆盖新文件的状态，
  // 否则"界面展示 A 的账号、实际导入 B 的同下标记录"。关闭重开时同理。
  const previewTokenRef = useRef(0);
  const [fileName, setFileName] = useState("");
  const [fileText, setFileText] = useState("");
  const [preview, setPreview] = useState<ImportPreviewAccount[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [parsing, setParsing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (open) {
      previewTokenRef.current += 1;
      setFileName("");
      setFileText("");
      setPreview([]);
      setSelected(new Set());
      setParsing(false);
      setBusy(false);
      setError("");
    }
  }, [open]);

  function chooseFile() {
    fileInputRef.current?.click();
  }

  function onFileChange(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    // 清空 value，允许再次选择同一文件
    e.target.value = "";
    if (!file) return;
    const token = ++previewTokenRef.current;
    const reader = new FileReader();
    reader.onerror = () => {
      if (token !== previewTokenRef.current) return;
      setParsing(false);
      setError("读取文件失败");
    };
    reader.onload = () => {
      if (token !== previewTokenRef.current) return;
      const text = String(reader.result ?? "");
      setFileName(file.name);
      setFileText(text);
      setParsing(true);
      setError("");
      api
        .previewImportAccounts(text)
        .then((res) => {
          if (token !== previewTokenRef.current) return;
          setPreview(res.accounts);
          setSelected(new Set(res.accounts.map((a) => a.index)));
        })
        .catch((err) => {
          if (token !== previewTokenRef.current) return;
          setPreview([]);
          setSelected(new Set());
          setError(api.asError(err));
        })
        .finally(() => {
          if (token === previewTokenRef.current) setParsing(false);
        });
    };
    reader.readAsText(file);
  }

  const allSelected = preview.length > 0 && selected.size === preview.length;

  function toggleAll() {
    setSelected(allSelected ? new Set() : new Set(preview.map((a) => a.index)));
  }

  function toggle(index: number) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(index)) next.delete(index);
      else next.add(index);
      return next;
    });
  }

  async function doImport() {
    if (busy || parsing || !fileText || selected.size === 0) return;
    setBusy(true);
    setError("");
    try {
      const res = await api.importAccounts(fileText, [...selected]);
      onImported?.(res);
      onOpenChange(false);
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="min-w-0 overflow-x-hidden">
        <DialogHeader>
          <DialogTitle>导入账号</DialogTitle>
          <DialogDescription>
            选择 JSON 文件，勾选要导入的账号。
            {variant === "ai" &&
              `备份中的国内版与国际版账号按各自档位归类，不随当前${variantLabel(variant)}改变。`}
          </DialogDescription>
        </DialogHeader>

        <input
          ref={fileInputRef}
          type="file"
          accept=".json,application/json"
          className="hidden"
          onChange={onFileChange}
        />

        <div className="flex items-center gap-2">
          <Button variant="outline" onClick={chooseFile} disabled={busy}>
            <FileUp />
            选择文件
          </Button>
          {fileName && <span className="truncate text-xs text-muted-foreground">{fileName}</span>}
        </div>

        {parsing && (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <Loader2 className="animate-spin" /> 正在解析…
          </div>
        )}

        {!parsing && fileText && preview.length === 0 && !error && (
          <p className="py-4 text-sm text-muted-foreground">
            文件解析成功，但里面没有可导入的账号（每条记录需要 id 与 payload 字段）。
          </p>
        )}

        {!parsing && preview.length > 0 && (
          <>
            <div className="flex items-center justify-between text-sm">
              <span className="text-muted-foreground">
                共 {preview.length} 个账号，已选 {selected.size} 个
              </span>
              <button type="button" className="cursor-pointer text-primary hover:underline" onClick={toggleAll}>
                {allSelected ? "取消全选" : "全选"}
              </button>
            </div>
            <div className="max-h-56 space-y-1 overflow-y-auto pr-1">
              {preview.map((a) => (
                <label
                  key={a.index}
                  className="flex cursor-pointer items-center gap-3 rounded-md border px-3 py-2 hover:bg-accent/50"
                >
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={selected.has(a.index)}
                    onChange={() => toggle(a.index)}
                  />
                  <span className="min-w-0 flex-1 truncate text-sm">{previewLabel(a)}</span>
                  {!a.hasToken && <Badge variant="outline">缺少 token</Badge>}
                </label>
              ))}
            </div>
          </>
        )}

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            取消
          </Button>
          <Button onClick={doImport} disabled={busy || parsing || selected.size === 0}>
            {busy ? "导入中…" : "导入勾选账号"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
