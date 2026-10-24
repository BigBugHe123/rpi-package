import { useEffect, useState } from "react";
import { Folder, GitBranch, GitFork } from "lucide-react";
import { api, type GitStatus, type Project } from "./api";
import { translate, type Language } from "./i18n";

export function ProjectContext({
  project,
  tick,
  language,
  loadGit,
  onOpen,
}: {
  project?: Project;
  tick: number;
  language: Language;
  loadGit: boolean;
  onOpen: () => void;
}) {
  const [status, setStatus] = useState<{ id: string; git: GitStatus } | null>(
    null,
  );
  useEffect(() => {
    if (!project || !loadGit) return;
    let cancelled = false;
    void api
      .gitStatus(project.id)
      .then((git) => {
        if (!cancelled) setStatus({ id: project.id, git });
      })
      .catch(() => {
        if (!cancelled) setStatus(null);
      });
    return () => {
      cancelled = true;
    };
  }, [project?.id, loadGit, tick]);
  const git = status?.id === project?.id ? status?.git : null;
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  return (
    <div className="composer-workspace" aria-label={t("projectContext")}>
      <button
        type="button"
        className="composer-workspace-path"
        title={project?.path}
        aria-label={project?.path ?? t("workspace")}
        onClick={onOpen}
      >
        <Folder size={12} />
        <span>{project?.path ?? t("workspace")}</span>
      </button>
      {git?.isRepository && git.branch && (
        <span
          className="composer-workspace-branch"
          title={`${t("gitBranch")}: ${git.branch}`}
          aria-label={`${t("gitBranch")}: ${git.branch}`}
        >
          <GitBranch size={12} />
          <span>{git.branch}</span>
        </span>
      )}
      {git?.isWorktree && (
        <span
          className="composer-workspace-worktree"
          title={git.worktreePath ?? project?.path}
        >
          <GitFork size={12} />
          <span>worktree</span>
        </span>
      )}
    </div>
  );
}
