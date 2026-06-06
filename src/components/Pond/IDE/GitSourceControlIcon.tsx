/** VS Code–style source-control icon (shared by Pond IDE + Collab activity bars). */
export default function GitSourceControlIcon({ size = 22 }: { size?: number }) {
  return (
    <span className="activity-icon activity-icon--git" aria-hidden>
      <svg width={size} height={size} viewBox="0 0 24 24" fill="none" strokeWidth="1.75">
        <circle className="git-node git-node-top" cx="6" cy="6" r="2.25" />
        <circle className="git-node git-node-bottom" cx="6" cy="18" r="2.25" />
        <circle className="git-node git-node-merge" cx="18" cy="12" r="2.25" />
        <path
          className="git-branch"
          d="M6 8.25v7.5M8.25 6h5.5a2.25 2.25 0 0 1 2.25 2.25v3.5"
          strokeLinecap="round"
        />
      </svg>
    </span>
  );
}
