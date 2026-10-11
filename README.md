# Ditok

App pessoal de ditado por voz para Windows e macOS: seguras um atalho, falas, largas e o texto aparece na app em foco. Vive na bandeja do sistema e só abre uma janela quando pedes as definições.

Feito com Tauri v2 (Rust) e uma UI mínima em TypeScript.

## Usar

| Ação | Atalho |
| - | - |
| Ditar (push-to-talk) | segurar **Ctrl+Shift+Espaço** (Mac: **Cmd+Shift+Espaço**) e largar no fim |
| Ditar (modo alternar) | tocar no atalho (<300 ms), falar, tocar outra vez |
| Cancelar a gravação | **Esc** |

O ícone do tray mostra o estado com a mascote: parado, a ouvir (vermelho), a processar (laranja), pronto (verde).

Os logs ficam em:
- Windows: `%LOCALAPPDATA%\com.manuelbrito.ditok\logs`
- macOS: `~/Library/Logs/com.manuelbrito.ditok`

## Desenvolver

Requisitos: Rust estável, Node 20+, pnpm.

```bash
pnpm install
pnpm tauri dev
```

```bash
cd src-tauri && cargo test
```

Os ícones são gerados a partir das SVG em `design/mascot/`:

```bash
pnpm icons
```

```bash
pnpm tauri icon src-tauri/icons/app-icon.png
```

## Limitações conhecidas

### Windows: apps abertas como administrador
O Windows impede que um processo normal envie teclas a uma janela com privilégios elevados (UIPI). O atalho funciona, mas a colagem falha em apps abertas "como administrador" (por exemplo, um terminal elevado). A alternativa é correr também o Ditok como administrador.

### Builds sem assinatura
A app não é assinada (é para uso pessoal), por isso o sistema avisa na primeira execução.

- **Windows (SmartScreen)**: em "O Windows protegeu o computador", clica em **Mais informações → Executar mesmo assim**.
- **macOS (Gatekeeper)**: clica com o botão direito na app → **Abrir** → **Abrir**. Em alternativa:
  ```bash
  xattr -dr com.apple.quarantine /Applications/Ditok.app
  ```
  No macOS, a app também precisa de autorização em **Definições do Sistema → Privacidade e Segurança** para **Microfone** e **Acessibilidade** (esta última serve para simular o Cmd+V).
