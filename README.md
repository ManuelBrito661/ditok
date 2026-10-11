# Ditok

App pessoal de ditado por voz para Windows e macOS: seguras um atalho, falas, largas e o texto aparece na app em foco. Vive na bandeja do sistema, mostra o estado numa pequena barra com a mascote e só abre uma janela quando pedes as definições.

Feito com Tauri v2 (Rust) e uma UI mínima em TypeScript. A transcrição é feita através do [OpenRouter](https://openrouter.ai).

## Usar

| Ação | Atalho |
| - | - |
| Ditar (push-to-talk) | segurar **Ctrl+Shift+Espaço** (Mac: **Cmd+Shift+Espaço**) e largar no fim |
| Ditar (modo alternar) | tocar no atalho (<300 ms), falar, tocar outra vez |
| Cancelar a gravação | **Esc** |

O atalho pode ser mudado nas definições.

### A barra com a mascote

Em baixo ao centro do ecrã (no monitor onde está o cursor) há uma pílula pequena com a mascote. Durante o ditado cresce e mostra o que está a acontecer:

| Estado | O que mostra |
| - | - |
| Em repouso | pílula minúscula com a mascote |
| A ouvir | barras que seguem o volume da tua voz e o tempo de gravação (fica laranja aos 45 s, perto do limite de 60 s do OpenRouter) |
| A processar | três pontos animados |
| Colado | contorno verde durante ~1 s |
| Falhou | contorno vermelho; uma notificação explica o motivo |

A barra nunca recebe o foco, por isso o texto é sempre colado na app em que estavas, e os cliques passam através dela. Pode ser escondida nas definições.

O ícone do tray mostra o mesmo estado: parado, a ouvir (vermelho), a processar (laranja), pronto (verde).

## Configurar

Clica no ícone do Ditok no tray → **Abrir definições**. A janela só existe enquanto está aberta: ao fechá-la é destruída, para libertar a RAM do WebView, e a app continua no tray. Cada alteração é guardada logo, sem botão "Guardar".

| Secção | O que tem |
| - | - |
| Geral | atalho (com deteção de conflitos com outras apps), idioma, arranque com o sistema, mostrar a barra com a mascote, guardar a última gravação |
| Transcrição | chave do OpenRouter (guardar / testar / apagar), modelo de transcrição com pesquisa e preço, formato de envio (multipart ou JSON) |
| Limpeza com IA | corrige pontuação e remove hesitações com um modelo de texto (desligada por omissão; se falhar ou demorar mais de 6 s, cola o texto original) |
| Histórico | texto dos últimos ditados, só neste computador (desligado por omissão) |
| Métricas e custos | latência p50/p90, custo e ditados do mês, últimos ditados, RAM da app e das janelas |

### Chave de API
A chave do [OpenRouter](https://openrouter.ai/settings/keys) fica no cofre do sistema (Gestor de Credenciais do Windows / Keychain do macOS), nunca em ficheiros, e a janela nunca a mostra: só indica se está configurada. Ao guardar, a chave é validada no OpenRouter antes de ficar no cofre.

Em alternativa à janela, na pasta `src-tauri`:

```bash
cargo run --bin set-key
```

### Ficheiros

| Ficheiro | Onde (Windows) | O que tem |
| - | - | - |
| `config.json` | `%APPDATA%\com.manuelbrito.ditok\` | as definições |
| `metrics.jsonl` | `%APPDATA%\com.manuelbrito.ditok\` | uma linha por ditado: tempos, modelo e custo, **sem o texto** |
| `costs.json` | `%APPDATA%\com.manuelbrito.ditok\` | custo e número de ditados por mês |
| `history.json` | `%APPDATA%\com.manuelbrito.ditok\` | texto dos últimos ditados (só se o histórico estiver ligado) |
| `debug\last.wav` | `%APPDATA%\com.manuelbrito.ditok\` | a última gravação (só se a opção estiver ligada) |
| `models-*.json` | `%LOCALAPPDATA%\com.manuelbrito.ditok\` | lista de modelos do OpenRouter, guardada durante 24 h |

### Logs

- Windows: `%LOCALAPPDATA%\com.manuelbrito.ditok\logs` (um ficheiro por dia, 7 dias)
- macOS: `~/Library/Logs/com.manuelbrito.ditok`

Cada ditado regista uma linha `ditado` com a duração do áudio, o tempo de transcrição, de limpeza e de colagem, o total (do soltar a tecla ao texto colado) e o custo. A memória é registada no arranque, com a janela de definições aberta e depois de a fechar.

## Desempenho

Medido no Windows em build de debug, com `openai/whisper-large-v3-turbo`:

| | Valor |
| - | - |
| Arranque até o atalho funcionar | ~30 ms (a barra aparece ~0,3 s depois) |
| Soltar a tecla → texto colado (2–4 s de fala) | 0,6–1,1 s, quase tudo é o pedido ao OpenRouter |
| RAM privada em repouso | ~107 MB: ~10 MB do Ditok + ~97 MB do WebView da barra |
| RAM sem a barra | ~10 MB |

O WebView2 corre com a aceleração por GPU desligada, o que poupa ~55 MB e não se nota numa barra deste tamanho. A RAM mostrada é a memória privada (o mesmo número do Gestor de Tarefas), não o working set, que conta várias vezes as DLLs partilhadas pelos processos do WebView2.

## Desenvolver

Requisitos: Rust estável, Node 20+, pnpm.

```bash
pnpm install
pnpm tauri dev
```

```bash
cd src-tauri && cargo test
```

Os ícones da app e do tray são gerados a partir das SVG em `design/mascot/`:

```bash
pnpm icons
```

```bash
pnpm tauri icon src-tauri/icons/app-icon.png
```

### Estrutura

```
src-tauri/src/
  lib.rs              arranque: plugins, tray, atalho, barra, estado partilhado
  dictation/          orquestrador (actor), máquina de estados do atalho, pipeline do ditado
  audio/              captura (cpal + ring buffer), mono 16 kHz, WAV, nível do microfone
  stt/                trait SttProvider, OpenRouter, cache da lista de modelos
  cleanup.rs          limpeza opcional com LLM
  paste.rs            clipboard + Ctrl/Cmd+V + reposição do clipboard
  hotkey.rs           atalho global, troca com deteção de conflitos
  overlay.rs          barra flutuante com a mascote
  settings_window.rs  janela de definições criada/destruída a pedido
  commands.rs         comandos Tauri chamados pela UI
  config.rs, secrets.rs, metrics.rs, history.rs, sysmem.rs, tray.rs, logging.rs
src/
  main.ts, api.ts     janela de definições (api.ts tem os tipos dos comandos)
  overlay.ts          barra com a mascote
design/mascot/        poses da mascote em SVG
```

## Limitações conhecidas

### Windows: apps abertas como administrador
O Windows impede que um processo normal envie teclas a uma janela com privilégios elevados (UIPI). O atalho funciona, mas a colagem falha em apps abertas "como administrador" (por exemplo, um terminal elevado). A alternativa é correr também o Ditok como administrador.

### Esc durante a gravação
Enquanto gravas, o Esc fica reservado ao Ditok para cancelar e não chega às outras apps. Fora da gravação não é intercetado.

### macOS ainda não testado
O código para o macOS (Cmd+V, Keychain, permissão do microfone, barra transparente) está escrito mas ainda não foi compilado nem testado num Mac. Também falta uma verificação explícita da permissão de Acessibilidade com uma mensagem clara.

### Builds sem assinatura
A app não é assinada (é para uso pessoal), por isso o sistema avisa na primeira execução.

- **Windows (SmartScreen)**: em "O Windows protegeu o computador", clica em **Mais informações → Executar mesmo assim**.
- **macOS (Gatekeeper)**: clica com o botão direito na app → **Abrir** → **Abrir**. Em alternativa:
  ```bash
  xattr -dr com.apple.quarantine /Applications/Ditok.app
  ```
  No macOS, a app também precisa de autorização em **Definições do Sistema → Privacidade e Segurança** para **Microfone** e **Acessibilidade** (esta última serve para simular o Cmd+V).
