// Seletor do microfone, partilhado pelas definições (Geral) e pelo guia de
// primeira execução. O valor "" quer dizer "o predefinido do sistema" (null no
// config.json); os outros valores são o nome do microfone tal como o sistema o dá.

import { api, errorText, type InputDevice } from "./api";
import { app, save } from "./store";

/** Pede a lista ao sistema e preenche `select`. Devolve-a (vazia se falhar). */
export async function loadDevices(select: HTMLSelectElement): Promise<InputDevice[]> {
  let devices: InputDevice[] = [];
  let failed = "";
  try {
    devices = await api.listInputDevices();
  } catch (e) {
    failed = errorText(e);
  }
  const current = app.view.config.input_device;
  const def = devices.find((d) => d.is_default);
  const option = (value: string, label: string) => {
    const o = document.createElement("option");
    o.value = value;
    o.textContent = label;
    return o;
  };
  const options = [option("", def ? `Predefinido do sistema (${def.name})` : "Predefinido do sistema")];
  for (const d of devices) options.push(option(d.name, d.name));
  // O escolhido não está ligado agora: continua na lista para não se perder a escolha.
  if (current && !devices.some((d) => d.name === current)) {
    options.push(option(current, `${current} (desligado: uso o predefinido)`));
  }
  select.replaceChildren(...options);
  select.value = current ?? "";
  select.title = failed ? `Não consegui listar os microfones: ${failed}` : "";
  return devices;
}

/** Grava a escolha quando muda. `after`: corre depois de gravar (ex.: reabrir o medidor). */
export function bindDeviceSelect(select: HTMLSelectElement, after?: () => void) {
  select.addEventListener("change", async () => {
    await save({ input_device: select.value || null });
    after?.();
  });
}

/** Mantém o seletor igual ao que está guardado (ex.: mudou no outro seletor). */
export function syncDeviceSelect(select: HTMLSelectElement) {
  const value = app.view.config.input_device ?? "";
  if ([...select.options].some((o) => o.value === value)) select.value = value;
}
