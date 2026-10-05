window.DELM_FILM = Object.freeze({
  duration: DELM_TIMING.duration,
  installer: "npx delm-agent-plugin",
  prompt:
    "Build Neon Rush, a Geometry Dash-inspired game with responsive controls and synthwave music. Test the gameplay.",
  repository: "github.com/jerry2247/delm-agent-plugin",
  project: "~/neon-rush",
  claudeVersion: "v2.1.289",
  claudeModel: "Opus 5.5 · Claude Max",
  command: "/delm:run",
  commandDescription: "(delm) Build in parallel with collaborating Claude Code agents and deliver their changes to your project.",
  reply: "Two DeLM agents are working on Neon Rush. Their progress is on the board.",
  finalReply:
    "Neon Rush is ready in ~/neon-rush. Both agents' changes are applied, and all 25 gameplay and audio tests pass.",
  tasks: [
    { id: 1, title: "Physics + level" },
    { id: 2, title: "Soundtrack + effects" },
    { id: 3, title: "Integrate the game" },
    { id: 4, title: "Controller checks" },
  ],
});
