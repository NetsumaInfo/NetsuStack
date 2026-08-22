import { PortlyMark } from "./portly-mark";

export const repoUrl = "https://github.com/Melvynx/portly";
export const downloadUrl = `${repoUrl}/releases/latest/download/Portly-macOS.zip`;

const nav = [
  { href: "/#problem", label: "Why" },
  { href: "/#agents", label: "Agents" },
  { href: "/#app", label: "App" },
  { href: "/#capabilities", label: "Capabilities" },
  { href: "/linux", label: "Linux", id: "linux" as const },
];

export function SiteHeader({ current }: { current?: "linux" }) {
  return (
    <header className="site-header">
      <div className="header-inner">
        <a className="brand" href="/">
          <PortlyMark size={26} />
          Portly
        </a>
        <nav aria-label="Main">
          {nav.map((item) => (
            <a
              key={item.href}
              href={item.href}
              aria-current={current && item.id === current ? "page" : undefined}
            >
              {item.label}
            </a>
          ))}
        </nav>
        <div className="header-actions">
          <a
            className="header-source"
            href={repoUrl}
            aria-label="Source on GitHub"
          >
            <GithubMark size={16} />
          </a>
          <a className="btn btn-small" href={downloadUrl}>
            Download
          </a>
        </div>
      </div>
    </header>
  );
}

export function SiteFooter() {
  return (
    <footer className="site-footer">
      <div className="footer-inner">
        <div className="footer-brand">
          <PortlyMark size={28} />
          <div>
            <strong>Portly</strong>
            <span>Local servers, under control.</span>
          </div>
        </div>
        <nav aria-label="Footer">
          <a href="/linux">Linux</a>
          <a href={repoUrl}>Source</a>
          <a href={`${repoUrl}/releases`}>Releases</a>
          <a href="/privacy">Privacy</a>
          <a href={`${repoUrl}/blob/main/LICENSE`}>MIT</a>
        </nav>
        <p>
          Built by <a href="https://melvynx.com">Melvynx</a>
        </p>
      </div>
    </footer>
  );
}

export function GithubMark({ size }: { size: number }) {
  return (
    <svg
      aria-hidden="true"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
    >
      <path d="M12 .7A11.3 11.3 0 0 0 8.4 22.8c.6.1.8-.3.8-.6v-2.2c-3.4.7-4.1-1.4-4.1-1.4-.5-1.4-1.3-1.8-1.3-1.8-1.1-.7.1-.7.1-.7 1.2.1 1.8 1.2 1.8 1.2 1.1 1.8 2.8 1.3 3.5 1 .1-.8.4-1.3.8-1.6-2.7-.3-5.6-1.4-5.6-6A4.7 4.7 0 0 1 5.7 7.4c-.1-.3-.5-1.6.1-3.3 0 0 1-.3 3.5 1.3a12 12 0 0 1 6.3 0C18 3.7 19.1 4 19.1 4c.6 1.7.2 3 .1 3.3a4.7 4.7 0 0 1 1.2 3.2c0 4.7-2.9 5.7-5.6 6 .4.4.8 1.1.8 2.2v3.4c0 .3.2.7.8.6A11.3 11.3 0 0 0 12 .7Z" />
    </svg>
  );
}
