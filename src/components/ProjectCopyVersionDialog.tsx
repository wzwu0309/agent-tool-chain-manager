import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { GitCommitHorizontal, Loader2, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import * as api from "../lib/tauri";
import type { GitSkillHistory, ManagedCopyUpdatePlan, ProjectSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";

interface Props {
  projectId: string;
  skillName: string;
  variants: ProjectSkill[];
  onClose: () => void;
  onApplied: () => Promise<void> | void;
}

interface LoadedHistory {
  skillId: string;
  value: GitSkillHistory;
}

interface LoadedPreview {
  key: string;
  value: ManagedCopyUpdatePlan;
}

const shortOid = (oid: string) => oid.slice(0, 12);

/**
 * Explicit update for ONE owned Copy in ONE agent target. The preview is a
 * read-only Core calculation; the apply API independently verifies the
 * selected target and preflight DeploymentId + CommitOid + TreeOid.
 * No generic legacy "update from center" fallback or inferred adoption.
 */
export function ProjectCopyVersionDialog({
  projectId, skillName, variants, onClose, onApplied,
}: Props) {
  const { t } = useTranslation();
  const [variantIndex, setVariantIndex] = useState(0);
  const [selectedCommit, setSelectedCommit] = useState("");
  const [history, setHistory] = useState<LoadedHistory | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [preview, setPreview] = useState<LoadedPreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [applyError, setApplyError] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const [refreshNonce, setRefreshNonce] = useState(0);

  const variant = variants[variantIndex];
  const skillId = variant?.center_skill_id;
  const queryKey = useMemo(() =>
    variant ? [projectId, variant.agent, variant.relative_path, selectedCommit].join("\u0000") : "",
    [projectId, variant, selectedCommit]
  );
  const activeHistory = history?.skillId === skillId ? history.value : null;
  const activePreview = preview?.key === queryKey ? preview.value : null;

  useEffect(() => {
    if (!skillId) return;
    let cancelled = false;
    setHistory(null);
    setHistoryError(null);
    setHistoryLoading(true);
    void api.getProjectSkillGitHistory(skillId).then((value) => {
      if (!cancelled) setHistory({ skillId, value });
    }).catch((e: unknown) => {
      if (!cancelled) setHistoryError(getErrorMessage(e, t("common.error")));
    }).finally(() => {
      if (!cancelled) setHistoryLoading(false);
    });
    return () => { cancelled = true; };
  }, [skillId, t]);

  useEffect(() => {
    if (!variant || !skillId) return;
    let cancelled = false;
    setPreview(null);
    setPreviewError(null);
    setPreviewLoading(true);
    void api.planProjectSkillCopyUpdate(
      projectId, variant.relative_path, variant.agent, selectedCommit || undefined,
    ).then((value) => {
      if (!cancelled) setPreview({ key: queryKey, value });
    }).catch((e: unknown) => {
      if (!cancelled) setPreviewError(getErrorMessage(e, t("common.error")));
    }).finally(() => {
      if (!cancelled) setPreviewLoading(false);
    });
    return () => { cancelled = true; };
  }, [projectId, variant, skillId, selectedCommit, queryKey, refreshNonce, t]);

  useEffect(() => {
    if (applying) return;
    const keydown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, [applying, onClose]);

  const apply = async () => {
    if (!activePreview || applying || previewLoading || historyLoading || historyError) return;
    setApplying(true);
    setApplyError(null);
    try {
      await api.applyProjectSkillCopyUpdate(activePreview);
    } catch (e) {
      // Keep the actual mutation failure visible even while the preview
      // reloads. A failed optimistic preview is never an authorization.
      setApplyError(getErrorMessage(e, t("common.error")));
      setPreview(null);
      setRefreshNonce((value) => value + 1);
      setApplying(false);
      return;
    }
    // After the Core commit, a UI refresh failure must not be reported as a
    // failed transaction. Commit success and presentation are distinct.
    toast.success(t("project.gitUpdate.success"));
    onClose();
    try {
      await onApplied();
    } catch (e) {
      toast.error(getErrorMessage(e, t("common.error")));
    }
  };

  return createPortal(
    <div className="fixed inset-0 z-[60] flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label={t("project.gitUpdate.title")}>
      <div className="absolute inset-0 bg-black/70 backdrop-blur-sm" onClick={() => { if (!applying) onClose(); }} />
      <section className="relative flex w-full max-w-lg flex-col rounded-xl border border-border bg-surface p-5 shadow-2xl max-h-[calc(85vh/var(--app-scale))] overflow-y-auto">
        <div className="mb-4 flex items-center justify-between">
          <h3 className="flex items-center gap-2 text-[14px] font-semibold text-primary">
            <GitCommitHorizontal className="h-4 w-4" />
            {t("project.gitUpdate.title")} · {skillName}
          </h3>
          <button type="button" onClick={onClose} disabled={applying} aria-label={t("common.cancel")} className="rounded-md p-1 text-muted hover:text-secondary disabled:opacity-50">
            <X className="h-4 w-4" />
          </button>
        </div>
        <p className="mb-4 text-[12px] text-muted">{t("project.gitUpdate.scope")}</p>

        <label className="mb-1 text-[12px] font-medium text-secondary" htmlFor="copy-update-agent">
          {t("project.gitUpdate.agent")}
        </label>
        <select
          id="copy-update-agent"
          className="app-input mb-3 w-full text-[13px]"
          value={variantIndex}
          disabled={applying}
          onChange={(e) => {
            setVariantIndex(Number(e.target.value));
            setSelectedCommit("");
            setPreview(null);
            setApplyError(null);
          }}
        >
          {variants.map((item, index) => (
            <option key={item.agent + ":" + item.path} value={index}>
              {item.agent_display_name} · {item.relative_path}
            </option>
          ))}
        </select>

        <label className="mb-1 text-[12px] font-medium text-secondary" htmlFor="copy-update-commit">
          {t("project.gitUpdate.commit")}
        </label>
        {historyLoading ? (
          <p className="mb-3 flex items-center gap-2 text-[12px] text-muted">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            {t("project.gitUpdate.loading")}
          </p>
        ) : historyError ? (
          <p className="mb-3 text-[12px] text-amber-600 dark:text-amber-400">{historyError}</p>
        ) : (
          <select
            id="copy-update-commit"
            className="app-input mb-3 w-full text-[13px]"
            value={selectedCommit}
            disabled={applying || !activeHistory}
            onChange={(e) => {
              setSelectedCommit(e.target.value);
              setApplyError(null);
            }}
          >
            <option value="">{t("project.gitUpdate.latest")}</option>
            {activeHistory?.versions.filter((item) => !item.isHead).map((item) => (
              <option value={item.commitOid} key={item.commitOid}>
                {shortOid(item.commitOid)} · {item.message || item.committedAt}
              </option>
            ))}
          </select>
        )}
        {(activeHistory?.uncommittedCentralChangesExcluded || activePreview?.uncommittedCentralChangesExcluded) && (
          <p className="mb-3 text-[12px] text-amber-600 dark:text-amber-400">{t("project.gitUpdate.dirty")}</p>
        )}

        {previewLoading && (
          <p className="my-3 flex items-center gap-2 text-[12px] text-muted">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            {t("project.gitUpdate.checking")}
          </p>
        )}
        {previewError && <p role="alert" className="my-3 break-words text-[12px] text-amber-600 dark:text-amber-400">{previewError}</p>}
        {applyError && <p role="alert" className="my-3 break-words text-[12px] text-amber-600 dark:text-amber-400">{applyError}</p>}
        {activePreview && (
          <div className="my-3 space-y-1 rounded-lg border border-border-subtle p-3 text-[12px] text-secondary">
            <p className="font-medium">{t("project.gitUpdate.preview")}</p>
            <p className="break-all">{t("project.gitUpdate.path")}: {activePreview.targetPath}</p>
            <p>{t("project.gitUpdate.from")}: <code>{shortOid(activePreview.baseCommitOid)}</code></p>
            <p>{t("project.gitUpdate.to")}: <code>{shortOid(activePreview.targetCommitOid)}</code></p>
            <p>{activePreview.willChangeContent
              ? t("project.gitUpdate.contentChanges")
              : activePreview.willChangeVersion
                ? t("project.gitUpdate.versionOnly")
                : t("project.gitUpdate.current")}</p>
            {activePreview.willChangeContent && (
              <p className="mt-2 text-muted">{t("project.gitUpdate.retainedBackup")}</p>
            )}
          </div>
        )}
        <div className="mt-4 flex justify-end gap-2">
          <button type="button" onClick={onClose} disabled={applying} className="rounded-lg px-3 py-1.5 text-[13px] text-secondary hover:bg-surface-hover disabled:opacity-50">
            {t("common.cancel")}
          </button>
          <button
            type="button"
            onClick={() => void apply()}
            disabled={!activePreview || !activePreview.willChangeVersion || previewLoading || historyLoading || !!historyError || applying}
            className="flex items-center gap-2 rounded-lg bg-accent px-4 py-1.5 text-[13px] font-medium text-white hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            {applying && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
            {t("project.gitUpdate.apply")}
          </button>
        </div>
      </section>
    </div>,
    document.body,
  );
}
