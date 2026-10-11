# Ditok

App pessoal de ditado por voz para Windows e macOS: seguras um atalho, falas, largas e o texto aparece na app em foco. Vive na bandeja do sistema (no macOS também na Dock e no Command-Tab), mostra o estado numa pequena barra com a mascote e tem uma janela, ao estilo do Wispr Flow, com os teus ditados, métricas e definições.

Feito com Tauri v2 (Rust) e uma UI mínima em TypeScript. A transcrição é feita através do [OpenRouter](https://openrouter.ai).

## Usar

| Ação | Atalho |
| - | - |
| Ditar (push-to-talk) | segurar **Ctrl+Shift+Espaço** (Mac: **Cmd+Shift+Espaço**) e largar no fim |
| Ditar (modo alternar) | tocar no atalho (<300 ms), falar, tocar outra vez |
| Ditar com o rato | clicar na barra com a mascote, falar, clicar no **■** |
| Cancelar a gravação | **Esc** (ou o **✕** na barra) |

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
| Não ouvi nada | durante ~1 s, quando a gravação só tinha silêncio ou ruído: não foi enviada nem colada |

A barra reage ao rato: ao passares por cima cresce e mostra uma dica com o atalho, e um clique começa a ditar em modo alternar. Enquanto ouve nesse modo mostra um **✕** (cancelar) e um **■** (parar e colar). Fora da pílula os cliques passam através da janela. A barra nunca recebe o foco, por isso o texto é sempre colado na app em que estavas. Pode ser escondida nas definições.

O ícone do tray mostra o mesmo estado: parado, a ouvir (vermelho), a processar (laranja), pronto (verde).

### Silêncio

Antes de enviar, o Ditok procura a voz na gravação. O silêncio do início e do fim é cortado (com uma margem de 0,3 s antes e 0,4 s depois), porque os modelos são cobrados ao segundo. Se não houver voz nenhuma, nada é enviado: quando só recebem silêncio, os modelos de transcrição costumam inventar frases ("Obrigado por assistir.").

## A janela

Abre-se no tray → **Abrir o Ditok** ou, no macOS, clicando no ícone da Dock. A janela só existe enquanto está aberta: ao fechá-la é destruída, para libertar a RAM do WebView, e a app continua a correr. Tem modo claro e escuro.

| Página | O que tem |
| - | - |
| Início | ditados, palavras e custo do mês, e o histórico dos últimos ditados com um botão "Copiar" em cada um. Um ditado que não chegou a ser colado também fica lá |
| Métricas | latência p50/p90, custo e ditados por mês, últimos ditados, RAM da app e das janelas |

### Definições

No botão **Definições** da barra lateral. Cada alteração é guardada logo, sem botão "Guardar".

| Separador | O que tem |
| - | - |
| Geral | atalho (com deteção de conflitos com outras apps), idioma, arranque com o sistema, mostrar a barra com a mascote, deixar o ditado no clipboard depois de colar (ligado por omissão; desligado, o clipboard volta ao que lá tinhas) |
| Transcrição | chave do OpenRouter (guardar / testar / apagar), modelo de transcrição com pesquisa e preço, formato de envio (multipart ou JSON) |
| Limpeza com IA | corrige pontuação e remove hesitações com um modelo de texto (desligada por omissão; se falhar ou demorar mais de 6 s, cola o texto original) |
| Dados e privacidade | histórico (ligado por omissão, só neste computador), guardar a última gravação |

### Atualizações

O Ditok procura versões novas nas [Releases do GitHub](https://github.com/ManuelBrito661/ditok/releases) 10 s depois de arrancar e depois a cada 6 h. Quando há uma, mostra uma notificação e um cartão **Nova versão** na janela; o botão **Atualizar** descarrega, instala e reinicia a app. As atualizações são assinadas e verificadas antes de instalar.

### Chave de API
A chave do [OpenRouter](https://openrouter.ai/settings/keys) fica no cofre do sistema (Gestor de Credenciais do Windows / Keychain do macOS), nunca em ficheiros, e a janela nunca a mostra: só indica se está configurada. Ao guardar, a chave é validada no OpenRouter antes de ficar no cofre.

Em alternativa à janela, na pasta `src-tauri`:

```bash
cargo run --bin set-key --features cli
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

Cada ditado regista uma linha `ditado` com a duração do áudio enviado e do silêncio cortado, o tempo de transcrição, de limpeza e de colagem, o total (do soltar a tecla ao texto colado) e o custo. Uma gravação sem voz regista uma linha `sem voz na gravação` com o ruído de fundo, o pico e o limiar em dBFS, que servem para afinar a deteção se o teu microfone a enganar. A memória é registada no arranque, com a janela aberta e depois de a fechar.

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

### Publicar uma versão

O workflow [release.yml](.github/workflows/release.yml) compila para Windows e macOS (universal) quando é feito push de uma tag `v*`, e publica os instaladores e o `latest.json` usado pelas atualizações.

1. Subir a versão em `package.json`, `src-tauri/tauri.conf.json` e `src-tauri/Cargo.toml` (e correr `cargo check` para atualizar o `Cargo.lock`).
2. Commit `chore: release X.Y.Z`.
3. Criar e enviar a tag:

```bash
git tag vX.Y.Z && git push origin main vX.Y.Z
```

Secrets necessários no GitHub:

| Secret | Para quê |
| - | - |
| `TAURI_SIGNING_PRIVATE_KEY` | assina as atualizações (chave em `~/.tauri/ditok.key`; a pública está no `tauri.conf.json`) |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` | certificado autoassinado "Ditok Self-Signed" (`~/.tauri/ditok-codesign.p12`, em base64) para assinar a app no macOS |
| `KEYCHAIN_PASSWORD` | password do keychain temporário criado no CI do macOS |

**Não substituas o certificado do macOS.** O macOS associa a permissão do microfone à identidade de assinatura: com um certificado novo, volta a pedir a permissão a todos. Guarda uma cópia de `~/.tauri`.

### Estrutura

```
src-tauri/src/
  lib.rs              arranque: plugins, tray, atalho, barra, estado partilhado
  dictation/          orquestrador (actor), máquina de estados do atalho, pipeline do ditado
  audio/              captura (cpal + ring buffer), mono 16 kHz, deteção de voz, WAV, nível do microfone
  stt/                trait SttProvider, OpenRouter, cache da lista de modelos
  cleanup.rs          limpeza opcional com LLM
  paste.rs            clipboard + Ctrl/Cmd+V (+ reposição do clipboard, se pedida), copiar sem colar
  hotkey.rs           atalho global, troca com deteção de conflitos
  overlay.rs          barra flutuante com a mascote (hover e cliques)
  settings_window.rs  janela principal criada/destruída a pedido
  updater.rs          procura e instala atualizações
  commands.rs         comandos Tauri chamados pela UI
  config.rs, secrets.rs, metrics.rs, history.rs, sysmem.rs, tray.rs, logging.rs
src-tauri/Info.plist, Entitlements.plist   permissões do macOS (microfone)
src/
  main.ts             janela principal: navegação e cartão de atualização
  home.ts, metrics-page.ts, settings-dialog.ts   Início, Métricas e definições
  api.ts, store.ts    tipos dos comandos e estado partilhado
  overlay.ts          barra com a mascote
design/mascot/        poses da mascote em SVG
```

## Limitações conhecidas

### Windows: apps abertas como administrador
O Windows impede que um processo normal envie teclas a uma janela com privilégios elevados (UIPI). O atalho funciona, mas a colagem falha em apps abertas "como administrador" (por exemplo, um terminal elevado). A alternativa é correr também o Ditok como administrador.

### Esc durante a gravação
Enquanto gravas, o Esc fica reservado ao Ditok para cancelar e não chega às outras apps. Fora da gravação não é intercetado.

### macOS: permissão de Acessibilidade
Ainda falta uma verificação explícita da permissão de Acessibilidade com uma mensagem clara. Sem ela, a colagem falha.

### Builds sem assinatura reconhecida
A app é para uso pessoal, por isso não tem uma assinatura reconhecida pelo sistema e há um aviso na primeira execução. No Windows não é assinada. No macOS é assinada com um certificado autoassinado: chega para a permissão do microfone ficar guardada entre atualizações, mas a app não é notarizada pela Apple.

- **Windows (SmartScreen)**: em "O Windows protegeu o computador", clica em **Mais informações → Executar mesmo assim**.
- **macOS (Gatekeeper)**: clica com o botão direito na app → **Abrir** → **Abrir**. Em alternativa:
  ```bash
  xattr -dr com.apple.quarantine /Applications/Ditok.app
  ```
  No macOS, a app também precisa de autorização em **Definições do Sistema → Privacidade e Segurança** para **Microfone** e **Acessibilidade** (esta última serve para simular o Cmd+V).
