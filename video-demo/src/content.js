window.DELM_FILM = Object.freeze({
  duration: DELM_TIMING.duration,
  installer: "npx delm-agent-plugin",
  prompt:
    "Build Neon Rush, a Geometry Dash-inspired game with responsive controls and synthwave music. Test the gameplay.",
  repository: "github.com/jerry2247/delm-agent-plugin",
  stages: [
    { name: "Title", start: 0, end: 1.2 },
    { name: "Install", start: 1.2, end: 9.2 },
    { name: "Request", start: 9.2, end: 22.2 },
    { name: "Collaborate", start: 22.2, end: 44.2 },
    { name: "Result", start: 44.2, end: 50.2 },
    { name: "Close", start: 50.2, end: 56.2 },
  ],
});
