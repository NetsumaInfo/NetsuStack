import { createFileRoute } from "@tanstack/react-router";
import {
  GithubMark,
  SiteFooter,
  SiteHeader,
  repoUrl,
} from "../components/site-chrome";

declare const __PORTLY_VERSION__: string;

const linuxRepo = `${repoUrl}/tree/main/cli`;

export const Route = createFileRoute("/linux")({
  head: () => ({
    meta: [
      { title: "Linux CLI — Portly" },
      {
        name: "description",
        content:
          "Headless Portly for Linux: the same agent CLI as macOS, a loopback daemon, and systemd --user instead of a window.",
      },
      { property: "og:title", content: "Portly on Linux: same CLI, no window" },
      {
        property: "og:description",
        content:
          "Build the Go supervisor, install it on PATH, and let agents drive local servers through portly — not nohup.",
      },
      { property: "og:url", content: "https://portly.melvynx.dev/linux" },
    ],
    links: [
      { rel: "canonical", href: "https://portly.melvynx.dev/linux" },
    ],
  }),
  component: LinuxDocs,
});

const toc = [
  ["install", "Install"],
  ["forever", "Stay up"],
  ["agents", "Agent commands"],
  ["diff", "vs macOS"],
  ["paths", "Paths & API"],
] as const;

const commands = [
  ["status", "Active servers and problems. --details / --json when you need more."],
  ["temp / wait", "One-off job. temp returns an ID; wait prints logs and the real exit code (124 on timeout)."],
  ["add-project / add-server", "Long-lived work. --start launches it under the daemon."],
  ["start / stop / restart", "One server, a project, or --all."],
  ["logs", "Captured output. --tail N."],
  ["take-over", "Stop the stray listener on the configured port, then start Portly's command."],
  ["port / kill-port", "Who holds a port. SIGTERM only when you ask."],
  ["forever", "systemd --user unit. enable | status | disable."],
  ["open", "Succeeds with a no-UI message. There is no window."],
  ["quit", "Stops every managed server and the daemon."],
];

function LinuxDocs() {
  return (
    <>
      <SiteHeader current="linux" />
      <main id="main-content" className="doc">
        <header className="doc-hero">
          <p className="kicker">Linux</p>
          <h1>Same CLI. No window.</h1>
          <p className="lede">
            Linux runs the headless supervisor in{" "}
            <a className="doc-inline" href={linuxRepo}>
              cli/
            </a>. Do not install the SwiftUI app there. A <code>portly</code> command
            starts a loopback daemon on <code>127.0.0.1:7737</code> if needed,
            then talks to it. Agents get the same commands as macOS.
          </p>
        </header>

        <div className="doc-layout">
          <nav className="doc-toc" aria-label="On this page">
            {toc.map(([id, label]) => (
              <a key={id} href={`#${id}`}>
                {label}
              </a>
            ))}
          </nav>

          <div className="doc-body">
            <section id="install">
              <h2>Install</h2>
              <p>
                Needs Go 1.22+. The binary <em>is</em> the supervisor. Put it
                on <code>PATH</code>, then any later <code>portly</code> call
                can start the daemon.
              </p>
              <pre className="install-shell" aria-label="Linux install commands">
                <b>$</b> git clone {repoUrl}.git{"\n"}
                <b>$</b> cd portly/cli{"\n"}
                <b>$</b> go test ./...{"\n"}
                <b>$</b> go build -o portly .{"\n"}
                <b>$</b> sudo install -m 755 portly /usr/local/bin/portly{"\n"}
                {"\n"}
                <span className="ok">✓ portly {__PORTLY_VERSION__}</span>
                {"\n"}
                <span className="ok">✓ API on 127.0.0.1:7737</span>
              </pre>
              <p>
                Cross-compile from another host with{" "}
                <code>GOOS=linux GOARCH=amd64 go build -o portly .</code>
              </p>
            </section>

            <section id="forever">
              <h2>Stay up across logins</h2>
              <p>
                <code>forever</code> writes a systemd <strong>user</strong> unit
                and enables it. No LaunchAgent. If{" "}
                <code>systemctl --user</code> is missing, the command fails
                instead of silently installing the wrong thing.
              </p>
              <pre className="install-shell" aria-label="forever commands">
                <b>$</b> portly forever enable --json{"\n"}
                <b>$</b> portly forever status --json{"\n"}
                <b>$</b> loginctl enable-linger "$USER"
              </pre>
              <p>
                Linger matters on a headless VPS: without it, the user systemd
                instance dies when the SSH session ends. Also export{" "}
                <code>XDG_RUNTIME_DIR=/run/user/$(id -u)</code> in non-login
                agent environments so <code>systemctl --user</code> can talk to
                the bus.
              </p>
            </section>

            <section id="agents">
              <h2>What agents should run</h2>
              <p>
                Inspect first. Reuse a healthy managed server. One-off work is
                a temporary job, not <code>nohup</code> and not{" "}
                <code>&amp;</code>.
              </p>
              <pre className="install-shell" aria-label="Agent workflow">
                <b>$</b> portly status{"\n"}
                <b>$</b> job_id="$(portly temp 'pnpm test' --path /app --timeout 30m)"{"\n"}
                <b>$</b> portly wait "$job_id"{"\n"}
                {"\n"}
                <b>$</b> portly add-project --name app --path /app --json{"\n"}
                <b>$</b> portly add-server --project app --name web \{"\n"}
                {"    "}--command 'pnpm dev' --port 5173 --start --json
              </pre>
              <ul className="doc-commands">
                {commands.map(([name, body]) => (
                  <li key={name}>
                    <code>{name}</code>
                    <span>{body}</span>
                  </li>
                ))}
              </ul>
              <p>
                <code>temp</code> returns immediately. <code>wait</code> exits
                with the process code. A timeout kills the process group and
                exits <code>124</code>. Completed temp metadata stays about an
                hour and is never written to <code>config.json</code>.
              </p>
            </section>

            <section id="diff">
              <h2>What changes on Linux</h2>
              <ul className="doc-list">
                <li>
                  No app, no menu bar, no Sparkle. <code>open</code> is a
                  documented no-op.
                </li>
                <li>
                  <code>forever</code> is systemd user, not launchd.
                </li>
                <li>
                  Shell is <code>$SHELL -lc</code> when it is an absolute
                  executable, otherwise <code>/bin/bash -lc</code>.
                </li>
                <li>
                  PTY via creack/pty, with a pipe fallback if PTY setup fails.
                </li>
                <li>
                  Do not run the macOS app and this daemon on the same host.
                  Both claim <code>127.0.0.1:7737</code>.
                </li>
              </ul>
            </section>

            <section id="paths">
              <h2>Paths and the API</h2>
              <ul className="doc-list">
                <li>
                  Config: <code>~/.config/portly/config.json</code>
                </li>
                <li>
                  Logs: <code>~/.config/portly/logs/</code>
                </li>
                <li>
                  Override the home with <code>PORTLY_HOME</code>
                </li>
                <li>
                  API: loopback only, default port <code>7737</code>. It can
                  spawn processes, so it never binds to the network.
                </li>
              </ul>
              <p>
                Put the same Development servers rule you use on macOS in the
                repo <code>AGENTS.md</code>. On a VPS, point agents at{" "}
                <code>/usr/local/bin/portly</code>, not Portly.app.
              </p>
              <p>
                <a className="link" href={linuxRepo}>
                  <GithubMark size={14} />
                  Source in cli/
                </a>
              </p>
            </section>
          </div>
        </div>
      </main>
      <SiteFooter />
    </>
  );
}
