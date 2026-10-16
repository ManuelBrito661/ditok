# Ditok

App pessoal de ditado por voz para Windows e macOS: seguras um atalho, falas, largas e o texto aparece na app em foco. Vive na bandeja do sistema (no macOS também na Dock e no Command-Tab), mostra o estado numa pequena barra com a mascote e tem uma janela, ao estilo do Wispr Flow, com os teus ditados, notas de reuniões, métricas e definições. O modo comando tem ainda a sua mini-janela, o assistente, com conversa e histórico, e podes **perguntar sobre o ecrã**: um atalho tira uma fotografia, escolhes uma área, escreves (ou dizes) o que queres saber e a área segue para a IA com a pergunta.

Feito com Tauri v2 (Rust) e uma UI mínima em TypeScript. Tudo o que usa IA passa pelos modelos da [xAI](https://x.ai), com uma só chave: a transcrição (em tempo real enquanto falas), a limpeza do texto, o modo comando e os resumos.

## Usar

| Ação | Atalho |
| - | - |
| Ditar (push-to-talk) | segurar **Ctrl+Win** (Mac: a tecla **Fn**) e largar no fim |
| Ditar (modo alternar) | tocar no atalho (<300 ms), falar, tocar outra vez |
| Ditar com o rato | clicar na barra com a mascote, falar, clicar no **■** |
| Mover a barra | arrastar a pílula (fora dos botões) para outro sítio do ecrã |
| Cancelar a gravação | **Esc** (ou o **✕** na barra) |
| Modo comando | atalho próprio, ligado em **Definições → Geral** (ver abaixo) |
| Perguntar sobre o ecrã | **Ctrl+Shift+Alt+S** (Mac: **⌘⇧⌥S**), ligado por omissão (ver [abaixo](#perguntar-sobre-o-ecrã)) |
| Abrir o assistente | tray → **Abrir o assistente** (ver abaixo) |
| Histórico rápido | atalho próprio, sem nenhum por omissão (ver abaixo) |

O atalho de ditado pode ser mudado nas definições (também aceita só modificadores, como Ctrl+Alt).

No Mac, o Fn precisa da permissão **Monitorização de entrada** (Definições do Sistema → Privacidade e Segurança). Com a permissão **Acessibilidade** o Ditok engole o Fn e o Mac não abre os emojis; sem ela, põe *Pressionar 🌐 para* em **Não fazer nada** (Definições do Sistema → Teclado). No Windows, o Ctrl+Win usa um hook de teclado; não funciona sobre janelas abertas como administrador.

### A barra com a mascote

Por omissão, em baixo ao centro do ecrã (no monitor onde está o cursor) há uma pílula pequena com a mascote. Podes arrastá-la para onde quiseres: o sítio fica guardado e volta lá no arranque. Se o monitor onde a deixaste deixar de existir, volta ao centro em baixo; para a repor à mão, usa **Repor posição da barra** no menu do tray (ou clique direito na pílula). Durante o ditado cresce e mostra o que está a acontecer:

| Estado | O que mostra |
| - | - |
| Em repouso | pílula minúscula com a mascote |
| A ouvir | barras que seguem o volume da tua voz e o tempo de gravação (fica laranja aos 5 min: o texto aparece enquanto falas, mas uma gravação tão longa já não é um ditado) |
| A processar | três pontos animados |
| Colado | contorno verde durante ~1 s |
| Falhou | contorno vermelho; uma notificação explica o motivo |
| Não ouvi nada | durante ~1 s, quando a gravação só tinha silêncio ou ruído: não foi enviada nem colada |

A barra reage ao rato: ao passares por cima cresce e mostra uma dica com o atalho, e um clique começa a ditar em modo alternar. Enquanto ouve nesse modo mostra um **✕** (cancelar) e um **■** (parar e colar). Fora da pílula os cliques passam através da janela. A barra nunca recebe o foco, por isso o texto é sempre colado na app em que estavas. Pode ser escondida em **Definições → Geral**.

O ícone do tray mostra o mesmo estado: parado, a ouvir (vermelho), a processar (laranja), pronto (verde). O menu do tray tem também **Tentar de novo o último ditado**.

### Sons

Duas notas curtas a subir quando começa a ouvir, as mesmas a descer quando pára, e uma nota grave ao cancelar. São geradas pela app (sem ficheiros) e tocam numa thread à parte, por isso não atrasam o microfone. Podem ser desligadas em **Definições → Geral**.

Com colunas em vez de auscultadores, o som de início fica gravado. A deteção de voz sabe que ele tocou e ignora um som curto (até 0,3 s) no primeiro meio segundo, para uma gravação acidental só com o "plim" não ir para o modelo.

### Silêncio

Antes de enviar, o Ditok procura a voz na gravação. O silêncio do início e do fim é cortado (com uma margem de 0,3 s antes e 0,4 s depois), porque os modelos são cobrados ao segundo. Se não houver voz nenhuma, nada é enviado: quando só recebem silêncio, os modelos de transcrição costumam inventar frases ("Obrigado por assistir.").

### Comandos falados

Diz **"nova linha"** ou **"novo parágrafo"** (em inglês, "new line" e "new paragraph") entre pausas curtas: "Olá Pedro, nova linha, tudo bem?" dá

```
Olá Pedro
Tudo bem?
```

O comando é resolvido no computador, sem IA. Só conta quando está isolado por pausas, que os modelos marcam com uma vírgula ou um ponto: no meio de uma frase ("abriram uma nova linha de metro") fica escrito tal e qual. A vírgula antes do comando e a pontuação depois dele desaparecem, e a frase seguinte começa com maiúscula. Os comandos e a pontuação falada podem ser desligados na página **Dicionário**.

## A janela

Abre-se no tray → **Abrir o Ditok** ou, no macOS, clicando no ícone da Dock. A janela só existe enquanto está aberta: ao fechá-la é destruída, para libertar a RAM do WebView, e a app continua a correr. Tem modo claro e escuro, segue o do sistema, e a barra lateral encolhe para só os ícones em janelas estreitas.

Na primeira execução aparece um guia em três passos: a chave da xAI, o microfone (com um medidor de nível ao vivo) e uma experiência de ditado. Pode ser saltado em qualquer altura e não volta a aparecer.

Para mudar de página sem o rato: `Ctrl+1` a `Ctrl+5` (no macOS, `⌘1` a `⌘5`) abrem Início, Ficheiros, Notas, Dicionário e Métricas, pela ordem da barra lateral, e `Ctrl+,` (`⌘,`) abre as Definições. A dica de cada página, ao passar o rato, diz o atalho.

| Página | O que tem |
| - | - |
| Início | ditados, palavras e custo do mês, e o histórico dos últimos ditados (com pesquisa e um botão "Copiar" em cada um). Um ditado que não chegou a ser colado também fica lá |
| Dicionário | palavras, expansões, comandos falados e importar/exportar (ver abaixo) |
| Ficheiros | transcrever a gravação de uma aula ou um vídeo, arrastado para a janela, com resumo (ver abaixo) |
| Notas | gravar uma reunião ou uma chamada, com transcrição ao vivo (ver abaixo) |
| Métricas | latência p50/p90, custo e ditados por mês, orçamento, estado da chave, estatísticas de uso, últimos ditados, RAM da app e das janelas |
| Definições | Geral (atalhos, microfone, áudio do computador, sons), xAI e custos (chave e orçamento), Limpeza (nível e modelos de texto), Estilos por app e Dados. É uma página, não uma janela por cima: tem separadores na horizontal e cada alteração grava-se logo |

### Dicionário e expansões

Aplicados no computador logo a seguir à transcrição, sem rede: não acrescentam custo, e o tempo que levam não se nota (menos de 1 ms com umas dezenas de entradas).

- **Palavras**: corrigem como o modelo escreve um nome ou termo. À esquerda as formas que ele usa, separadas por vírgulas (`tauri, tori` → `Tauri`); vazio quer dizer a própria palavra e serve para acertar as maiúsculas (`Manuel Brito`). Só apanham palavras inteiras, sem distinguir maiúsculas.
- **Expansões**: dizes a frase e é colado o texto (`o meu email` → o endereço). O texto pode ter várias linhas.

Com a limpeza com IA ligada, as palavras vão no pedido ao modelo, para ele as escrever assim. As expansões não: o modelo recebe um marcador (`{{1}}`) no lugar de cada uma, que é trocado pelo texto depois da limpeza. Assim o teu email ou a tua morada nunca saem do computador, e o modelo não os pode "corrigir". Se o modelo estragar um marcador ou mexer nas mudanças de linha, é colado o texto sem limpeza.

**Importar e exportar** em JSON (o formato do Ditok), CSV (`tipo;de;para`, com BOM para o Excel abrir bem) ou TXT (uma palavra por linha, só vocabulário). Se uma palavra importada já existir com outro valor, escolhes entre manter a tua ou usar a do ficheiro.

### Modo comando

Seleciona um texto numa app, carrega no atalho de comando e diz o que queres: "torna isto mais formal", "traduz para inglês". A resposta abre no [assistente](#o-assistente), uma janela pequena onde a lês, pedes mudanças e a pões no sítio de onde veio. Sem seleção, a instrução escreve texto novo ("escreve um email a recusar a reunião"). Usa o modelo de texto da limpeza, mesmo com a limpeza desligada, e aceita seleções até 8000 caracteres. Liga-se em **Definições → Geral**, com o atalho próprio (por omissão `Ctrl+Shift+Alt+Space`).

Em **Onde aparece a resposta** (também em Definições → Geral) podes voltar ao modo sem janela: a resposta é colada logo por cima da seleção (ou no cursor, sem seleção), como antes do assistente existir.

### O assistente

Um popup sem moldura, de cantos arredondados e fundo ligeiramente translúcido (420×650), com a sua própria interface, onde o modo comando mostra a resposta. Segue o tema do sistema (claro ou escuro). Aparece junto ao canto direito do monitor onde estás, a meio da altura (para não tapar o texto que editas), fica por cima das outras apps e não ocupa lugar na barra de tarefas. Arrasta-se pela barra de cima e fecha-se com o ✕ ou com `Esc`.

- **A resposta chega à medida que é escrita.** O botão da caixa de texto passa a **Parar**, e o que já veio fica. Por cima do primeiro pedido fica o texto que tinhas selecionado, numa citação que se abre com um clique (ou, numa pergunta sobre o ecrã, a miniatura da captura, que se abre com um clique).
- **Por baixo de cada resposta:** **Copiar**, **Substituir seleção** (ou **Inserir no cursor**, se não havia seleção) e **Tentar de novo**. "Substituir" volta à app de onde deste o comando e cola lá a resposta (ver as limitações) e fecha o popup, para não ficar por cima do que acabaste de editar; a conversa fica no histórico. Os botões da última resposta estão sempre à vista; os das outras aparecem ao passar o rato. Um bloco de código tem o seu próprio **Copiar**.
- **A conversa continua.** Escreve na caixa de texto ("mais curto", "em inglês", "porquê?") ou, com a janela em primeiro plano, carrega no atalho de comando e diz o pedido: nesse caso não se copia nenhuma seleção, o que dizes é mais uma mensagem da conversa. Um comando dado noutra app abre uma conversa nova.
- **Histórico** (o botão ☰ ou `Ctrl+K`): as conversas agrupadas por dia (hoje, ontem, últimos 7 e 30 dias, mais antigas), com pesquisa no texto todo. Apagar pede um segundo clique, e **Apagar todas** também. A pesquisa não distingue maiúsculas.
- **Teclas:** `Enter` envia, `Shift+Enter` muda de linha, `Ctrl+N` abre uma conversa nova, `Ctrl+K` abre o histórico e `Esc` fecha o histórico (ou a janela, se a caixa de texto estiver vazia). No macOS é `⌘` em vez de `Ctrl`.

Fechar a janela não pára uma resposta a chegar nem perde a conversa: ela continua no Ditok e reabre-se em tray → **Abrir o assistente** (ou no próximo comando). As conversas ficam guardadas em `conversas\` na pasta de dados (um ficheiro por conversa, as últimas 200) **só com o histórico ligado** (Definições → Geral); desligado, a conversa só existe enquanto está aberta, e nada fica em disco. **Apagar histórico** (Definições → Dados) apaga também as conversas.

Cada resposta custa o que a xAI cobrar ao modelo de texto, com o custo exato, e entra nas Métricas e no orçamento do mês. Numa conversa comprida seguem para o modelo as mensagens mais recentes (até ~48 mil caracteres); o primeiro pedido, com o texto selecionado, vai sempre. O texto selecionado é tratado como dados: uma instrução escrita lá dentro não é obedecida.

### Perguntar sobre o ecrã

Carrega no atalho (por omissão `Ctrl+Shift+Alt+S`, no Mac `⌘⇧⌥S`; muda-se, ou desliga-se, em **Definições → Geral**; também está no menu do tray, em **Perguntar sobre o ecrã**, e carregar outra vez no atalho fecha a seleção). O ecrã onde está o cursor "congela" e escolhes **o que queres mostrar** à IA e **o que queres saber**, tudo no mesmo sítio:

1. **A área.** Arrasta um retângulo (depois podes movê-lo por dentro e ajustá-lo pelas pegas), ou escolhe o ecrã todo com um clique ou `Enter`. A etiqueta diz o tamanho, em píxeis, do que vai seguir.
2. **A pergunta.** Aparece ao lado da área, para escreveres. Ou **dizes-a**: o botão do microfone, ou o atalho de ditado de sempre (`Ctrl+Win`, `Fn` no Mac), escreve o que dizes na caixa (com o texto a aparecer à medida que falas) em vez de o colar numa app. Podes juntar escrita e voz, e corrigir antes de enviar. Começar a escrever sem ter escolhido nada, ou a ditar, escolhe o ecrã todo.
3. **Enviar.** `Enter` (ou o botão). Com a caixa vazia o botão diz **Explicar** e pergunta "o que se vê". `Esc`, o ✕ ou mudar de janela cancelam, e nada sai do computador.

A resposta abre no [assistente](#o-assistente), que mostra a miniatura da captura por cima do pedido e **continua a conversa sobre a mesma imagem** ("e como se corrige?", "traduz o que lá diz"). Com **Inserir no cursor** passas a resposta para a app onde estavas.

- **O que sai do computador.** Só a área escolhida, e só quando envias, para a xAI (o texto que lá estiver escrito é conteúdo a ler, nunca instruções: tal como com a seleção do modo comando, a IA não obedece ao que a imagem disser). O lado maior é reduzido a 2048 px; segue em PNG (ou em JPEG, se o PNG passar de 4 MB).
- **A fotografia nunca vai para o disco.** Vive na memória enquanto a janela de seleção está aberta e a miniatura enquanto a conversa está aberta no assistente. Uma conversa reaberta do histórico guarda a pergunta e a resposta, mas diz que a captura já não está guardada.
- **Modelo.** Tem de ver imagens. Por omissão é o da limpeza (**Definições → Limpeza → Modelo para capturas do ecrã** escolhe outro; a lista só mostra os que aceitam imagens). Se o modelo não as aceitar, a resposta diz-te onde mudar. O custo conta nas Métricas e no orçamento como o de qualquer resposta do assistente.
- **Um monitor de cada vez**: o do cursor, quando carregas no atalho. Se o atalho estiver ocupado por outra app, as definições dizem-no.
- **macOS**: precisa da permissão **Gravação do ecrã** (Definições do Sistema → Privacidade e Segurança). Sem ela, o macOS dá só o papel de parede; por isso o Ditok pergunta antes e, se faltar, pede-a e explica. Depois de a dar, fecha e abre o Ditok.

### Histórico rápido

Um atalho (sem nenhum por omissão; define-o em **Definições → Geral**) abre um menu do sistema junto ao cursor com os últimos ditados. O escolhido é colado na janela onde estavas. É um menu nativo, por isso não tem caixa de pesquisa: para isso há a página Início.

### Estilos por app

Em **Definições → Estilos por app** dizes que tom usar em cada app. O estilo é escolhido pela app onde estavas quando começaste a ditar:

| Estilo | Efeito |
| - | - |
| Predefinido | usa só o nível de limpeza global |
| Casual | frases curtas, sem formalidades (mensagens) |
| Formal | frases completas e pontuação cuidada (email, documentos) |
| Literal | nunca passa pelo modelo, seja qual for o nível (terminal, editor de código) |

Vêm regras iniciais para as apps mais comuns (WhatsApp, Discord, Slack e Telegram como Casual; Outlook e Word como Formal; terminais e VS Code como Literal). Podes acrescentar, mudar ou remover as tuas.

### Notas de reuniões

Na página **Notas** gravas uma reunião, uma chamada ou uma aula online. Escolhes o que gravar:

- **Reunião ou chamada**: o teu microfone (**Eu**) e o áudio do computador (**Outros**).
- **Só o áudio do computador**: um vídeo ou uma aula online.
- **Conversa presencial**: só o microfone.

A transcrição aparece ao vivo, com cada linha marcada como "Eu" ou "Outros". A gravação continua com a janela fechada, e ao reabri-la a página mostra onde ficou. Com colunas em vez de auscultadores, o microfone apanha também o som da outra pessoa; com auscultadores fica tudo limpo. O áudio do computador é captado da saída que escolheres em **Definições → Geral**.

No fim, cada sessão fica numa pasta com a transcrição e os metadados, e pode ser resumida (com o modelo de resumos) e exportada. Quando a app fecha a meio de uma reunião, a sessão é recuperada como interrompida na próxima abertura.

"Outros" não é separado por pessoa por omissão. Se marcares **Separar quem fala em "Outros" (Orador 1, 2…)**, no fim da reunião a faixa dos outros (guardada num ficheiro temporário, apagado logo a seguir) vai inteira à xAI com `diarize=true`, que devolve quem disse cada palavra; o Ditok junta as palavras em trechos, numera os oradores pela ordem em que começam a falar e dá a cada linha o orador que mais se sobrepõe a ela. Custa mais 0,10 US$ por hora da faixa. Tem de ser numa só chamada: o número de um orador só quer dizer o mesmo dentro de um mesmo pedido, por isso não se faz enunciado a enunciado. O limite são 90 minutos.

### Transcrever ficheiros

Arrasta a gravação de uma aula, de uma reunião ou um vídeo para a janela (ou usa **Ficheiros → Escolher ficheiro…**). Funciona com mp3, m4a, aac, wav, flac, ogg (vorbis), mp4, mov, mkv e outros contentores de áudio ou vídeo comuns. Opus (notas de voz do WhatsApp, webm) ainda não.

1. Antes de começar aparece a duração e o custo estimado (0,10 US$ por hora de áudio), com um aviso se passar o orçamento do mês.
2. O ficheiro é lido aos bocados e partido em pedaços de ~2,5 min, cortados na pausa mais silenciosa perto desse ponto, para não partir palavras. Vão até 3 pedaços ao mesmo tempo para a xAI. Um pedaço que falhe por um problema passageiro (rede, servidor ocupado) é repetido até duas vezes, com uma pausa de 2 s e 4 s. Os pedaços só com silêncio (um intervalo) não são enviados.
3. Uma barra mostra o progresso e o tempo que falta. Podes fechar a janela: o trabalho continua e há uma notificação no fim.
4. O texto fica na janela, com **Copiar texto** e **Guardar ao lado do original** (`aula.txt`, sem substituir nenhum ficheiro), e é guardada sempre uma cópia em `transcricoes\` na pasta de dados. Com **Resumir** obténs um resumo do texto, do tipo que escolheres.

Os pedaços são juntos pela ordem do ficheiro, com um parágrafo novo quando o anterior acaba numa frase completa. Um pedaço que falhe de vez fica assinalado no texto (`[não foi possível transcrever 25:00–27:30]`). Ao parar a meio fica o que já estava transcrito. Só o dicionário é aplicado ao texto: comandos falados e expansões são para os teus ditados, e a limpeza com IA não é usada. O custo conta para o mês e para o orçamento.

Uma aula de 1 h custa 0,10 US$.

### Resumos

Uma aula, uma reunião ou um ditado comprido podem ser resumidos com o modelo de texto, numa única chamada. O resumo usa um modelo próprio (em **Definições → Limpeza**) ou, se não tiver, o da limpeza. O texto é tratado como dados, não como instruções: uma aula com "ignora o que disseram antes" não muda o resumo. Uma hora de aula cabe num pedido; acima de um certo tamanho a app recusa, com a razão, em vez de cortar o texto em silêncio.

### A xAI

Todos os modelos são da xAI, com uma única chave (criada em [console.x.ai](https://console.x.ai)) que fica no cofre do sistema, na entrada `xai-api-key`:

| Para quê | Modelo | Como | Preço |
| - | - | - | - |
| Ditar e comandos | `grok-voice-transcribe-2.0` | WebSocket em `wss://api.x.ai/v1/stt` enquanto falas | 0,20 US$ por hora de fala |
| Tentar de novo, reprocessar, ficheiros, notas | `grok-voice-transcribe-2.0` | `POST /v1/stt` com o áudio completo | 0,10 US$ por hora |
| Separar oradores nas notas | `grok-voice-transcribe-2.0` | `POST /v1/stt` com `diarize=true` | 0,10 US$ por hora |
| Limpeza, comandos, resumos | `grok-4.20-0309-non-reasoning` por omissão, à escolha | `POST /v1/chat/completions` | 1,25 / 2,50 US$ por milhão de tokens |
| Perguntar sobre o ecrã | o da limpeza por omissão, ou um que veja imagens à escolha | `POST /v1/chat/completions` com a captura em `image_url` (base64) | o que a xAI cobrar, com a imagem em tokens de entrada |

Como o Ditok tira partido de a xAI ser o único fornecedor:

- **Streaming no ditado e no comando.** O áudio segue enquanto falas (PCM 16 kHz) e o texto que a xAI vai ouvindo aparece numa dica por cima da pílula. Ao largar o atalho só falta o fim da frase (`finalize`): num teste com 8 s de fala o texto final chegou **~0,2 s depois de largar**, contra ~0,65 s do pedido em lote. O português é pedido como `pt-PT` e o texto vem formatado (números, moedas e unidades), com as palavras do dicionário como `keyterm`.
- **Lote de recurso.** O WAV continua a ser gravado sempre. Se o stream falhar (sem rede, tempo esgotado, texto vazio), o ditado cai no lote com o WAV completo.
- **Custo exato.** Nos pedidos de texto a xAI devolve `cost_in_usd_ticks` (1 US$ = 10^10 ticks), o que cobrou mesmo, já com o desconto da cache de prompts. Não se estima nada. A transcrição é cobrada à duração.
- **Cache de prompts.** O prompt de sistema repete-se a cada ditado; o cabeçalho `x-grok-conv-id` (um por finalidade: limpeza, comando, resumo) ajuda a xAI a reaproveitá-lo, e esses tokens saem mais baratos.
- **Modelos que raciocinam.** Os `grok-4.5` e seguintes não deixam desligar o raciocínio; o Ditok pede o esforço mais baixo (`reasoning_effort: low`) e dá folga ao limite de tokens. O padrão, `non-reasoning`, responde logo. A lista de modelos vem de `GET /v1/language-models` (com preços), guardada 24 h.
- **Uma ligação aquecida.** Ao premir o atalho abre-se (ou reaproveita-se) a ligação HTTP/2 à xAI quando vai haver limpeza ou comando, para o pedido não pagar o handshake.

#### Permissões da chave

As chaves da xAI têm **permissões por serviço**, escolhidas ao criá-las na consola. Uma chave criada só para a voz transcreve mas é recusada no chat (`403 Access to the chat endpoint is denied`), e a limpeza, os comandos e os resumos deixam de funcionar. **Definições → xAI e custos → Testar** (e o guardar da chave) verifica os dois serviços com 0,1 s de silêncio e uma resposta de um token, e diz qual está recusado. A correção está em console.x.ai: editar a chave para lhe dar acesso a todos os serviços (ou criar outra).

Quando a limpeza falha por um problema destes, o texto cola-se na mesma, sem limpeza, e aparece um aviso (no máximo uma vez a cada 10 minutos).

### Estatísticas de uso

Em **Métricas → Uso**: palavras por dia, a sequência de dias seguidos, um mapa de calor das últimas 53 semanas e as apps onde mais ditas. Só contagens: o texto nunca é guardado aqui. Há um botão para apagar estas estatísticas (os custos e os tempos ficam).

### Orçamento

Em **Definições → xAI e custos** podes definir um orçamento mensal em USD. Quando um ditado faz o gasto do mês passar os 80 % aparece uma notificação, e outra ao chegar aos 100 %. Cada aviso aparece uma vez por mês e o Ditok continua a funcionar depois do limite. O gasto é o que a xAI cobra pela transcrição e pela limpeza (ver acima), somado por mês em UTC. No Início o valor fica âmbar a partir dos 80 % e vermelho aos 100 %, e nas Métricas há uma barra.

Uma chave normal da xAI não consegue perguntar quanto saldo lhe resta (isso é na consola), por isso as Métricas só dizem se há chave e levam à consola.

### Atualizações

O Ditok procura versões novas nas [Releases do GitHub](https://github.com/ManuelBrito661/ditok/releases) 10 s depois de arrancar e depois a cada 6 h. Quando há uma, mostra uma notificação e um cartão **Nova versão** na janela; o botão **Atualizar** descarrega, instala e reinicia a app. As atualizações são assinadas e verificadas antes de instalar.

### Chave de API

A chave da [xAI](https://console.x.ai) fica no cofre do sistema (Gestor de Credenciais do Windows / Keychain do macOS), nunca em ficheiros, e a janela nunca a mostra: só indica se está configurada. Ao guardar, a app valida a chave na xAI e só depois a põe no cofre. Uma chave recusada não é guardada; se não houver rede, é guardada sem validação e fica registado no log. Depois de guardada, a app testa os dois serviços (voz e texto) e diz se a chave tem permissões em falta.

A entrada antiga do OpenRouter (`openrouter-api-key`) já não é usada e fica no cofre até a apagares tu (no Windows, em *Gestor de Credenciais*; no macOS, no *Acesso às Chaves*).

Em alternativa à janela, na pasta `src-tauri`:

```bash
cargo run --bin set-key --features cli
```

## Desempenho

Medido no Windows em build de debug, com a xAI:

| | Valor |
| - | - |
| Arranque até o atalho funcionar | ~30 ms (a barra aparece ~0,3 s depois) |
| Soltar a tecla → texto final (streaming, 8 s de fala) | ~0,2 s, só falta o fim da frase |
| O mesmo em lote (`POST /v1/stt`, 8 s de fala) | ~0,65 s, upload e transcrição |
| RAM privada em repouso | ~107 MB: ~10 MB do Ditok + ~97 MB do WebView da barra |
| RAM sem a barra | ~10 MB |

O WebView2 corre com a aceleração por GPU desligada, o que poupa ~55 MB e não se nota numa barra deste tamanho. A RAM mostrada é a memória privada (o mesmo número do Gestor de Tarefas), não o working set, que conta várias vezes as DLLs partilhadas pelos processos do WebView2.

## Ficheiros e logs

| Ficheiro | Onde (Windows) | O que tem |
| - | - | - |
| `config.json` | `%APPDATA%\com.manuelbrito.ditok\` | as definições, incluindo o dicionário, as expansões e os estilos por app |
| `metrics.jsonl` | `%APPDATA%\com.manuelbrito.ditok\` | uma linha por ditado: tempos, modelo e custo, **sem o texto** |
| `costs.json` | `%APPDATA%\com.manuelbrito.ditok\` | custo e número de ditados por mês |
| `usage.json` | `%APPDATA%\com.manuelbrito.ditok\` | contagens de palavras por dia e por app (sem texto) |
| `history.json` | `%APPDATA%\com.manuelbrito.ditok\` | texto dos últimos ditados (só se o histórico estiver ligado) |
| `conversas\` | `%APPDATA%\com.manuelbrito.ditok\` | as conversas do assistente, um ficheiro cada, as últimas 200 (só se o histórico estiver ligado). Numa pergunta sobre o ecrã fica só o tamanho da captura, nunca a imagem |
| `audio\` | `%APPDATA%\com.manuelbrito.ditok\` | o áudio dos últimos 20 ditados (só se a opção estiver ligada) |
| `transcricoes\` | `%APPDATA%\com.manuelbrito.ditok\` | uma cópia do texto de cada ficheiro transcrito |
| `notas\` | `%APPDATA%\com.manuelbrito.ditok\` | as sessões de reuniões: metadados, transcrição, resumo |
| `debug\last.wav` | `%APPDATA%\com.manuelbrito.ditok\` | a última gravação (só se a opção estiver ligada) |
| `models-xai.json` | `%LOCALAPPDATA%\com.manuelbrito.ditok\` | cache da lista de modelos de texto da xAI, guardada durante 24 h |

Logs:

- Windows: `%LOCALAPPDATA%\com.manuelbrito.ditok\logs` (um ficheiro por dia, 7 dias)
- macOS: `~/Library/Logs/com.manuelbrito.ditok`

Cada ditado regista uma linha `ditado` com a duração do áudio enviado e do silêncio cortado, o tempo de transcrição, de limpeza e de colagem, o total (do soltar a tecla ao texto colado), o custo e quantos comandos, palavras e expansões foram aplicados (o texto nunca vai para os logs). Uma gravação sem voz regista uma linha `sem voz na gravação` com o ruído de fundo, o pico e o limiar em dBFS, que servem para afinar a deteção se o teu microfone a enganar. A memória é registada no arranque, com a janela aberta e depois de a fechar.

## Desenvolver

Requisitos: Rust estável, Node 24 (o CI usa esta versão), pnpm.

```bash
pnpm install
pnpm tauri dev
```

Verificações (são as mesmas que o CI corre em Windows e macOS):

```bash
pnpm build
```

```bash
cd src-tauri && cargo fmt --check && cargo test
```

```bash
cd src-tauri && cargo clippy --all-targets -- -D warnings
```

O clippy passa sem avisos, mas no CI ainda não bloqueia (`continue-on-error`).

Os ícones da app e do tray são gerados a partir das SVG em `design/mascot/`:

```bash
pnpm icons
```

```bash
pnpm tauri icon src-tauri/icons/app-icon.png
```

### Publicar uma versão

O workflow [release.yml](.github/workflows/release.yml) compila para Windows e macOS (universal) quando é feito push de uma tag `v*`, e publica os instaladores e o `latest.json` usado pelas atualizações. Antes de compilar confirma que a versão da tag coincide com `Cargo.toml`, `tauri.conf.json` e `package.json`.

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
  state.rs            estado partilhado entre os módulos
  dictation/          orquestrador (actor), máquina de estados do atalho, pipeline do ditado,
                      falhas, seleção
  command.rs          modo comando: o prompt e a resposta sem janela (`command_output: paste`)
  assistant/          o assistente: conversa e streaming (mod.rs), conversas em disco (store.rs),
                      popup transparente (window.rs)
  screen/             perguntar sobre o ecrã: sessão e envio (mod.rs), fotografia do monitor com o
                      xcap (grab.rs), recorte e codificação (shot.rs), janela de seleção (window.rs)
  text.rs             comandos falados, dicionário e expansões (no computador)
  dict_io.rs          importar e exportar o dicionário (JSON, CSV, TXT)
  audio/              captura (cpal + ring buffer), mono 16 kHz, deteção de voz, WAV,
                      leitura de ficheiros (symphonia) e cortes nas pausas
  xai/                o único fornecedor de modelos: cliente (mod.rs), transcrição em lote e em
                      streaming (stt.rs), texto e conversas em streaming (chat.rs), lista de
                      modelos com cache (models.rs)
                      e repetição de pedidos (retry.rs)
  notes/              notetaker: as duas faixas (microfone e sistema), segmentação, sessões,
                      armazenamento, separação de oradores (xAI) e verificações
  files.rs            transcrever um ficheiro: pedaços em paralelo, progresso, texto final
  cleanup.rs          limpeza opcional com LLM
  summarize.rs        resumos de transcrições (uma chamada ao modelo de texto)
  paste.rs            clipboard + Ctrl/Cmd+V (+ reposição do clipboard, se pedida), copiar sem colar
  focus.rs            janela em foco, para confirmar antes de colar, e voltar à app de origem
                      ("Substituir" do assistente)
  quick_history.rs    menu do histórico rápido
  sound.rs            sons de início e fim, gerados na app
  hotkey.rs           atalho global, troca com deteção de conflitos
  modkeys.rs, modkeys/  Fn (macOS) e Ctrl+Win (Windows) por hook de teclado
  overlay.rs          barra flutuante com a mascote (hover, cliques, posição guardada em overlay.json)
  settings_window.rs  janela principal criada/destruída a pedido
  usage.rs            estatísticas de uso (palavras por dia, sequências, apps)
  updater.rs          procura e instala atualizações
  commands.rs         comandos Tauri chamados pela UI
  config.rs, secrets.rs, metrics.rs, history.rs, sysmem.rs, tray.rs, logging.rs
  bin/set-key.rs      guardar a chave da xAI (feature `cli`)
  bin/notes-probe.rs  ferramenta de diagnóstico das notas (feature `cli`)
src-tauri/Info.plist, Entitlements.plist   permissões do macOS (microfone)
src/
  main.ts             janela principal: navegação e cartão de atualização
  home.ts, dictionary-page.ts, dictionary-add.ts, files-page.ts, notes-page.ts,
  metrics-page.ts, settings-page.ts, styles-panel.ts
                      Início, Dicionário, Ficheiros, Notas, Métricas e definições
  onboarding.ts       guia de primeira execução
  assistant.ts, assistant-history.ts, assistant-text.ts, assistant-icons.ts, assistant.css
                      a janela do assistente (assistant.html)
  screen.ts, screen.css   a seleção da captura do ecrã (screen.html)
  devices.ts          seletor de microfone (partilhado)
  summary.ts          resumos (Ficheiros e Notas)
  api.ts, store.ts, format.ts   tipos dos comandos, estado partilhado e formatação
  overlay.ts, overlay.css       barra com a mascote
  styles.css
design/mascot/        poses da mascote em SVG
scripts/gen-icons.mjs geração dos ícones (pnpm icons)
.github/workflows/    ci.yml (Windows e macOS) e release.yml (tags v*)
```

## Limitações conhecidas

### Windows: apps abertas como administrador
O Windows impede que um processo normal envie teclas a uma janela com privilégios elevados (UIPI). O atalho funciona, mas a colagem falha em apps abertas "como administrador" (por exemplo, um terminal elevado). A alternativa é correr também o Ditok como administrador.

### Assistente: translúcido, mas sem desfoque
O fundo do popup é translúcido, mas não desfocado: uma página web só desfoca o que está dentro da sua própria janela, não o ambiente de trabalho por trás dela. O desfoque a sério ("vidro fosco") pede um efeito nativo do sistema (Acrylic no Windows, vibrancy no macOS), que tem custos próprios (no Windows, arrastar a janela fica pesado e os cantos ficam quadrados fora do Windows 11). Por isso o fundo tem 88 % de opacidade, o que deixa perceber o que está por trás sem atrapalhar a leitura (`--glass` em `src/assistant.css`).

### Assistente: "Substituir" e a app de origem
"Substituir" volta à app de onde deste o comando e cola lá a resposta. No Windows volta à **janela**; se a fechaste (ou se era uma app aberta como administrador), o texto fica no clipboard e o botão avisa. No macOS volta à **app** (pelo número do processo, porque as janelas não se verificam) e precisa da mesma permissão de Acessibilidade da colagem. Uma conversa que reabras do histórico, ou que comeces a escrever na própria janela, já não sabe de onde veio: só tem **Copiar**.

### Perguntar sobre o ecrã: um monitor, e janelas protegidas
Só se fotografa o monitor onde está o cursor. No Windows a fotografia é feita por GDI: o conteúdo que uma app protege (vídeo com DRM, algumas janelas de bancos) pode vir preto, como numa captura de ecrã normal. A fotografia é tirada no instante em que carregas no atalho: tudo o que estiver visível (um menu aberto, uma dica) sai nela, e o que se mexe (um vídeo) fica parado. No macOS é preciso a permissão **Gravação do ecrã** (ver acima), e a primeira vez o sistema pergunta.

### Esc durante a gravação
Enquanto gravas, o Esc fica reservado ao Ditok para cancelar e não chega às outras apps. Fora da gravação não é intercetado.

### macOS: permissão de Acessibilidade
Ainda não há uma verificação prévia da permissão de Acessibilidade com uma mensagem clara. A app só explica o que falta quando a colagem falha.

### Notas: "Outros" sem separar por pessoa
Por omissão, todo o áudio do computador é "Outros". A separação por orador (xAI) é opcional e feita só no fim da reunião.

### Builds sem assinatura reconhecida
A app é para uso pessoal, por isso não tem uma assinatura reconhecida pelo sistema e há um aviso na primeira execução. No Windows não é assinada. No macOS é assinada com um certificado autoassinado: chega para a permissão do microfone ficar guardada entre atualizações, mas a app não é notarizada pela Apple.

- **Windows (SmartScreen)**: em "O Windows protegeu o computador", clica em **Mais informações → Executar mesmo assim**.
- **macOS (Gatekeeper)**: clica com o botão direito na app → **Abrir** → **Abrir**. Em alternativa:
  ```bash
  xattr -dr com.apple.quarantine /Applications/Ditok.app
  ```
  No macOS, a app também precisa de autorização em **Definições do Sistema → Privacidade e Segurança** para **Microfone** e **Acessibilidade** (esta última serve para simular o Cmd+V).
