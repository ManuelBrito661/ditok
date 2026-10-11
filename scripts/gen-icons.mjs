// Gera os ícones da app e do tray a partir das SVG da mascote em design/mascot/.
// Uso: pnpm icons  (depois corre `pnpm tauri icon src-tauri/icons/app-icon.png`)
import fs from "node:fs";
import path from "node:path";
import { Resvg } from "@resvg/resvg-js";

const root = path.resolve(import.meta.dirname, "..");
const mascot = (name) => fs.readFileSync(path.join(root, "design/mascot", `${name}.svg`), "utf8");

function render(svg, size, out) {
  const png = new Resvg(svg, { fitTo: { mode: "width", value: size } }).render().asPng();
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.writeFileSync(out, png);
  console.log(`${path.relative(root, out)} (${size}px, ${png.length} B)`);
}

// Ícone da app: pose "parado" sobre um círculo azul-claro, como no design.
{
  const svg = mascot("parado")
    .replace(/viewBox="[^"]*"/, 'viewBox="-20 -10 440 440"')
    .replace("<!-- BODY:START", '<circle cx="200" cy="210" r="218" fill="#DCE6FF"/>\n  <!-- BODY:START');
  render(svg, 1024, path.join(root, "src-tauri/icons/app-icon.png"));
}

// Ícones do tray: a 16-32 px os detalhes das poses perdem-se, por isso retiramos os
// elementos decorativos (class="pose-extra"), recortamos ao corpo e juntamos um
// distintivo de cor no canto, que é o que realmente se distingue na barra.
const badge = {
  idle: "",
  recording: '<circle cx="306" cy="298" r="54" fill="#E5484D" stroke="#fff" stroke-width="12"/>',
  processing:
    '<circle cx="306" cy="298" r="54" fill="#F5A524" stroke="#fff" stroke-width="12"/>' +
    '<circle cx="282" cy="298" r="9" fill="#fff"/><circle cx="306" cy="298" r="9" fill="#fff"/><circle cx="330" cy="298" r="9" fill="#fff"/>',
  done:
    '<circle cx="306" cy="298" r="54" fill="#5DC47B" stroke="#fff" stroke-width="12"/>' +
    '<path d="M282 298 L300 316 L332 281" stroke="#fff" stroke-width="16" stroke-linecap="round" stroke-linejoin="round" fill="none"/>',
};
const poseFor = { idle: "parado", recording: "a-ouvir", processing: "a-processar", done: "pronto" };

for (const [state, pose] of Object.entries(poseFor)) {
  const svg = mascot(pose)
    .replace(/<g class="pose-extra"[\s\S]*?<\/g>/g, "")
    .replace(/<[a-z]+ class="pose-extra"[^>]*\/>/g, "")
    .replace(/viewBox="[^"]*"/, 'viewBox="30 18 345 345"')
    .replace("</svg>", `${badge[state]}</svg>`);
  render(svg, 64, path.join(root, `src-tauri/icons/tray/${state}.png`));
}
