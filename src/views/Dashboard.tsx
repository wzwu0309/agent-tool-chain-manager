import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { Layers, CheckCircle2, Bot, Plus, Download, AlertTriangle, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useApp } from "../context/AppContext";
import { AgentControlSetupCard } from "../components/AgentControlSetupCard";
import * as api from "../lib/tauri";
import type { ProjectSkillRecovery } from "../lib/tauri";
import { recoveryNeedsAttention, recoveryNeedsFollowUp, recoveryMayBeInProgress } from "../lib/projectRecovery";

export function Dashboard() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { tools, projects, managedSkills, openSkillDetailById } = useApp();

  const [activeRecoveries, setActiveRecoveries] = useState<ProjectSkillRecovery[]>([]);
  const [activeRecoveryError, setActiveRecoveryError] = useState(false);
  const [startupRecoveryScanFailed, setStartupRecoveryScanFailed] = useState(false);
  const activeRecoveryRequestRef = useRef(0);
  const loadActiveRecoveries = useCallback(async () => {
    const request = ++activeRecoveryRequestRef.current;
    try {
      const records = await api.getActiveProjectSkillRecoveries();
      if (request === activeRecoveryRequestRef.current) {
        setActiveRecoveries(records);
        setActiveRecoveryError(false);
      }
    } catch (error) {
      console.error("Failed to load active project recovery inbox:", error);
      if (request === activeRecoveryRequestRef.current) setActiveRecoveryError(true);
    }
  }, []);

  useEffect(() => {
    const requestGuard = activeRecoveryRequestRef;
    void loadActiveRecoveries();
    return () => { requestGuard.current++; };
  }, [loadActiveRecoveries, projects.length]);

  const [orphanedRecoveries, setOrphanedRecoveries] = useState<ProjectSkillRecovery[]>([]);
  const [orphanError, setOrphanError] = useState(false);
  const [orphanLoading, setOrphanLoading] = useState(false);
  const orphanRequestRef = useRef(0);
  const loadOrphans = useCallback(async () => {
    const request = ++orphanRequestRef.current;
    setOrphanLoading(true);
    try {
      const records = await api.getUnregisteredProjectSkillRecoveries();
      if (request === orphanRequestRef.current) {
        setOrphanedRecoveries(records);
        setOrphanError(false);
      }
    } catch (error) {
      console.error("Failed to load unregistered project recovery log:", error);
      if (request === orphanRequestRef.current) setOrphanError(true);
    } finally {
      if (request === orphanRequestRef.current) setOrphanLoading(false);
    }
  }, []);

  useEffect(() => {
    // A project detached from the Sidebar disappears from the active list,
    // so refresh this independent read-only recovery index immediately.
    const requestGuard = orphanRequestRef;
    void loadOrphans();
    return () => { requestGuard.current++; };
  }, [loadOrphans, projects.length]);

  useEffect(() => {
    let disposed = false;
    const listener = listen<{ ok: boolean }>("project-skill-recovery-inspection-finished", (event) => {
      if (disposed) return;
      setStartupRecoveryScanFailed(!event.payload.ok);
      void loadActiveRecoveries();
      void loadOrphans();
    }).then((stop) => {
      // Also read once *after* subscription is attached: otherwise the
      // background scan can finish between the first query and listen().
      if (!disposed) {
        void loadActiveRecoveries();
        void loadOrphans();
      }
      return stop;
    });
    return () => {
      disposed = true;
      void listener.then((stop) => stop()).catch(() => {});
    };
  }, [loadActiveRecoveries, loadOrphans]);

  const actionRecoveries = activeRecoveries.filter(recoveryNeedsFollowUp);
  const urgentRecoveries = actionRecoveries.filter(recoveryNeedsAttention);

  const enabledAgents = useMemo(
    () => tools.filter((tool) => tool.installed && tool.enabled),
    [tools]
  );

  const totalSkills = managedSkills.length;
  const syncedSkills = useMemo(
    () => managedSkills.filter((s) => s.targets.length > 0).length,
    [managedSkills]
  );

  const divergedCount = useMemo(
    () => projects.reduce((acc, p) => acc + p.sync_health.diverged, 0),
    [projects]
  );

  const recentSkills = useMemo(
    () => [...managedSkills].sort((a, b) => b.updated_at - a.updated_at).slice(0, 5),
    [managedSkills]
  );

  const coverageLabel = totalSkills === 0 ? "0" : `${syncedSkills}/${totalSkills}`;
  const syncCardIcon = divergedCount > 0 ? AlertTriangle : CheckCircle2;
  const syncCardColor = divergedCount > 0 ? "text-amber-600 dark:text-amber-400" : "text-emerald-600 dark:text-emerald-400";
  const syncCardBg = divergedCount > 0 ? "bg-amber-500/[0.08]" : "bg-emerald-500/[0.08]";

  return (
    <div className="app-page app-page-narrow">
      <div className="app-page-header">
        <h1 className="app-page-title">{t("dashboard.greeting")}</h1>
        <p className="app-page-subtitle text-tertiary">
          {t("dashboard.summary", {
            skills: totalSkills,
            agents: enabledAgents.length,
            projects: projects.length,
          })}
        </p>
      </div>

      <AgentControlSetupCard />

      {(actionRecoveries.length > 0 || activeRecoveryError || startupRecoveryScanFailed) && (
        <section role="alert" className="app-panel border border-amber-500/40 p-4">
          <div className="flex items-center justify-between gap-2">
            <h2 className="flex items-center gap-2 text-[13px] font-semibold text-amber-700 dark:text-amber-300">
              <AlertTriangle className="h-4 w-4" />
              {t("dashboard.recoveryInbox.title", { count: actionRecoveries.length })}
            </h2>
            <button type="button" onClick={() => { void loadActiveRecoveries(); void loadOrphans(); }}
              className="rounded-md p-1.5 text-muted hover:bg-surface-hover"
              aria-label={t("common.refresh")} title={t("common.refresh")}>
              <RefreshCw className="h-4 w-4" />
            </button>
          </div>
          <p className="mt-1 text-[12px] text-secondary">
            {urgentRecoveries.length > 0
              ? t("dashboard.recoveryInbox.mayBeUnavailable")
              : t("dashboard.recoveryInbox.readOnly")}
          </p>
          {(activeRecoveryError || startupRecoveryScanFailed) && (
            <p className="mt-2 text-[12px] text-amber-700 dark:text-amber-300">
              {t("dashboard.recoveryInbox.statusUncertain")}
            </p>
          )}
          <div className="mt-3 space-y-2">
            {actionRecoveries.map((record) => (
              <div key={record.operation_id} className="rounded-md border border-border p-2 text-[12px]">
                <p className="font-medium text-secondary">
                  {projects.find((project) => project.id === record.project_id)?.name ?? record.project_id}
                  {" · "}{record.kind}{" · "}{t("project.recovery.phase." + record.phase, { defaultValue: record.phase })}
                </p>
                <p className="mt-1 text-muted">
                  {recoveryMayBeInProgress(record)
                    ? t("project.recovery.mayBeInterrupted")
                    : t("project.recovery.manualReview")}
                </p>
                <p className="mt-1 break-all text-muted">
                  {t("project.recovery.target")}: {record.target_path}
                  {" · "}{record.target_exists
                    ? t("project.recovery.targetPresent") : t("project.recovery.targetMissing")}
                </p>
                {record.retained_paths.length > 0 && (
                  <p className="mt-1 break-all text-muted">
                    {t("project.recovery.retainedPaths")}: {record.retained_paths.join(", ")}
                  </p>
                )}
                <button type="button"
                  className="mt-1 rounded-lg border border-border px-2 py-1 text-secondary hover:bg-surface-hover"
                  onClick={() => navigate("/project/" + encodeURIComponent(record.project_id))}>
                  {t("dashboard.recoveryInbox.inspect")}
                </button>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Stats */}
      <div className="grid grid-cols-3 gap-3.5">
        {[
          {
            title: t("dashboard.librarySkills"),
            value: String(totalSkills),
            icon: Layers,
            color: "text-accent-light",
            bg: "bg-accent-bg",
          },
          {
            title: t("dashboard.syncCoverage"),
            value: coverageLabel,
            icon: syncCardIcon,
            color: syncCardColor,
            bg: syncCardBg,
          },
          {
            title: t("dashboard.connectedAgents"),
            value: String(enabledAgents.length),
            icon: Bot,
            color: "text-sky-600 dark:text-sky-400",
            bg: "bg-sky-500/[0.08]",
          },
        ].map((stat, i) => {
          const Icon = stat.icon;
          return (
            <div
              key={i}
              className="app-panel flex items-center justify-between px-4 py-4 transition-colors hover:border-border"
            >
              <div>
                <p className="app-section-title mb-1">
                  {stat.title}
                </p>
                <h3 className="text-[17px] font-semibold text-primary leading-none">{stat.value}</h3>
              </div>
              <div className={`p-2 rounded-md ${stat.bg} ${stat.color} border border-border-subtle`}>
                <Icon className="w-4 h-4" />
              </div>
            </div>
          );
        })}
      </div>

      {(orphanedRecoveries.length > 0 || orphanError) && (
        <section className="app-panel border border-border p-4" aria-label={t("dashboard.orphanedRecovery.title")}>
          <div className="flex items-center justify-between gap-3">
            <h2 className="text-[13px] font-semibold text-secondary">
              <AlertTriangle className="mr-1.5 inline-block h-4 w-4 text-amber-600 dark:text-amber-400" />
              {t("dashboard.orphanedRecovery.title")} ({orphanedRecoveries.length})
            </h2>
            <button
              type="button"
              onClick={() => void loadOrphans()}
              disabled={orphanLoading}
              className="rounded-md p-1.5 text-muted hover:bg-surface-hover disabled:opacity-50"
              title={t("common.refresh")}
              aria-label={t("common.refresh")}
            >
              <RefreshCw className={"h-4 w-4" + (orphanLoading ? " animate-spin" : "")} />
            </button>
          </div>
          <p className="mt-1 text-[12px] text-muted">{t("dashboard.orphanedRecovery.readOnly")}</p>
          {orphanError && (
            <p role="alert" className="mt-2 text-[12px] text-amber-600 dark:text-amber-400">
              {t("dashboard.orphanedRecovery.error")}
            </p>
          )}
          <div className="mt-3 max-h-52 space-y-2 overflow-y-auto">
            {orphanedRecoveries.map((record) => (
              <div key={record.operation_id} className="rounded-md border border-border-subtle p-2 text-[12px]">
                <p className="font-medium text-secondary">{record.kind} · {record.phase}</p>
                <p className="mt-1 break-all text-muted">ProjectId: {record.project_id}</p>
                <p className="mt-1 break-all text-muted">{t("dashboard.orphanedRecovery.target")}: {record.target_path}</p>
                <p className="mt-1 break-all text-muted">{t("dashboard.orphanedRecovery.recovery")}: {record.recovery_root}</p>
                <p className="mt-1 text-muted">{record.recovery_root_exists
                  ? t("dashboard.orphanedRecovery.present")
                  : t("dashboard.orphanedRecovery.missing")}</p>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Actions */}
      <div className="flex gap-3">
        <button
          onClick={() => navigate("/install?tab=local")}
          className="app-button-primary flex-1"
        >
          <Download className="w-4 h-4" />
          {t("dashboard.scanImport")}
        </button>
        <button
          onClick={() => navigate("/install")}
          className="app-button-secondary flex-1"
        >
          <Plus className="w-4 h-4 text-tertiary" />
          {t("dashboard.installNew")}
        </button>
      </div>

      {/* Recent skills */}
      {recentSkills.length > 0 && (
        <div>
          <h2 className="app-section-title mb-2.5">
            {t("dashboard.recentActivity")}
          </h2>
          <div className="app-panel overflow-hidden divide-y divide-border-subtle">
            {recentSkills.map((skill) => (
              <div
                key={skill.id}
                role="button"
                tabIndex={0}
                onClick={() => {
                  openSkillDetailById(skill.id);
                  navigate("/my-skills");
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    openSkillDetailById(skill.id);
                    navigate("/my-skills");
                  }
                }}
                className="flex items-center justify-between px-3.5 py-2.5 hover:bg-surface-hover transition-colors cursor-pointer"
              >
                <div className="flex items-center gap-2.5">
                  <div className="w-6 h-6 rounded-md flex items-center justify-center text-[13px] font-semibold bg-accent-bg text-accent-light shrink-0">
                    {skill.name.charAt(0).toUpperCase()}
                  </div>
                  <div>
                    <h4 className="text-[13px] text-secondary font-medium flex items-center gap-1.5">
                      {skill.name}
                      <span className="text-[11px] px-1.5 py-px rounded-full bg-surface-hover text-muted border border-border font-normal">
                        {skill.source_type}
                      </span>
                    </h4>
                    <p className="text-[13px] text-muted mt-px">
                      {skill.targets.length > 0
                        ? `${t("dashboard.synced")} → ${skill.targets.map((target) => target.tool).join(", ")}`
                        : t("dashboard.notSynced")}
                    </p>
                  </div>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
