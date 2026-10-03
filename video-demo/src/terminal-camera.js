import { PerspectiveCamera, Scene, MathUtils } from "three";
import { CSS3DObject, CSS3DRenderer } from "three/addons/renderers/CSS3DRenderer.js";

const rig = document.querySelector("#installer-rig");
const terminal = document.querySelector("#installer");
const scene = new Scene();
const camera = new PerspectiveCamera(30, 1, 1, 10000);
const renderer = new CSS3DRenderer();
const surface = new CSS3DObject(terminal);
const cameraDistance = 3400;
const settleSeconds = 1.05;
let width = 0;
let height = 0;

camera.position.z = cameraDistance;
scene.add(surface);
renderer.domElement.className = "terminal-camera";
Object.assign(renderer.domElement.style, {
  position: "absolute",
  inset: "0",
  overflow: "visible",
  pointerEvents: "none",
});
Object.assign(terminal.style, { left: "0", top: "0" });
rig.appendChild(renderer.domElement);

function draw(filmTime) {
  const nextWidth = rig.clientWidth;
  const nextHeight = rig.clientHeight;
  if (nextWidth !== width || nextHeight !== height) {
    width = nextWidth;
    height = nextHeight;
    renderer.setSize(width, height);
    terminal.style.width = `${width}px`;
    terminal.style.height = `${height}px`;
    camera.aspect = width / height;
    // The focal length equals the camera distance, preserving CSS pixel size
    // when the terminal is front-facing throughout the layout transitions.
    camera.fov = MathUtils.radToDeg(2 * Math.atan(height / (2 * cameraDistance)));
    camera.updateProjectionMatrix();
  }
  const progress = MathUtils.clamp(filmTime / settleSeconds, 0, 1);
  const eased = progress ** 3 * (progress * (progress * 6 - 15) + 10);
  const angle = 1 - eased;
  surface.rotation.set(MathUtils.degToRad(1.5 * angle), MathUtils.degToRad(-2 * angle), 0);
  renderer.render(scene, camera);
}

window.DELM_TERMINAL_CAMERA = Object.freeze({ draw });
draw(0);
