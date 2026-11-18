*Esta é uma tradução das notas das versões 0.2.1, 0.2.0 e 0.1.8. As versões anteriores estão em
[release_notes.md](release_notes.md) (inglês), que prevalece em caso de diferença.*

# romexplorer 0.3.0 (alpha)

Alfa: o esquema da base de dados pode mudar entre versões sem forma de atualização. Faça uma cópia
de segurança antes de substituir uma versão.

## Alterações

**Análise**
- Adicionada «Análise rápida (alterados)»: também lê os ficheiros que mudaram de tamanho ou data.
- Os hashes da análise definem-se em separado para ficheiros normais, ficheiros de arquivo e o conteúdo dos arquivos. Os ficheiros de arquivo usam sha1 e xxh3_128 por predefinição.

**Cópia**
- Uma cópia reiniciada verifica vários ficheiros já presentes no destino ao mesmo tempo.

**Listas**
- As vistas em cartões com muitas entradas abrem e deslizam mais depressa.
- Shift+clique seleciona um intervalo, como no Windows e no macOS. Um clique junto a uma caixa marca-a em vez de abrir a entrada.
- Seleção múltipla em mais listas: títulos combinados, tarefas do ecrã de análise, origens de DAT, ficheiros ROM, BIOS, ROM com patch, listas de material, versões definitivas e dispositivos com sessão iniciada.

**Progresso**
- Cada tarefa mostra o que está a fazer, o total e em que ponto vai, incluindo a pesquisa de pastas, as verificações e os passos da base de dados.
- As tarefas com vários passos mostram a lista de passos.
- As tarefas têm o botão «detalhes».
- As tarefas em curso mostram o tempo decorrido e o tempo restante.
- As transferências grandes e as verificações de ficheiros mostram quanto já foi lido.
- Esvaziar o catálogo e importar um ficheiro de coleções são executados como tarefas.
- A aplicação de computador mostra o que está a fazer enquanto arranca.

**Áudio**
- Várias faixas podem ser convertidas ao mesmo tempo para reprodução.

## Correções

- O progresso da análise de áudio parava durante a medição do volume.
- O progresso do índice de música das ROM só avançava a cada 500 ROM.

# romexplorer 0.2.2 (alpha)

Alfa: o esquema da base de dados pode mudar entre versões sem forma de atualização. Faça uma cópia
de segurança antes de substituir uma versão.

## Alterações

**Áudio das ROM**
- O índice de música das ROM pode ler várias ROM ao mesmo tempo (4 por predefinição, alterável nas definições de áudio).
- O índice de música das ROM lê só a cópia mais bem classificada de cada título, e as outras só se essa não tiver música.
- Nos jogos com vários discos, os ficheiros de música que um disco anterior já tem não voltam a ser lidos.
- Os jogos em cartucho cuja música a aplicação já conhece reproduzem-na assim que a página do título abre, sem esperar pelo índice.
- Adicionado «Limpar o índice de áudio das ROM» (todas ou por plataforma).

# romexplorer 0.2.1 (alpha)

Alfa: o esquema da base de dados pode mudar entre versões sem forma de atualização. Faça uma cópia
de segurança antes de substituir uma versão.

## Alterações

**3DS**
- As ROM de 3DS podem ser convertidas para outros formatos.
- As chaves não estão incluídas. Coloque aes_keys.txt (e seeddb.bin quando um título precisar) ao
  lado dos ficheiros, ao lado do romexplorer ou na sua pasta assets.
- Correspondência por número de série para as ROM de 3DS.

**Áudio das ROM**
- Ao abrir a página de um título, a música das ROM desse jogo passa a ser indexada primeiro.
- O índice de música das ROM passa a ler primeiro as ROM mais pequenas e a saltar as não
  suportadas.
- São suportados os jogos de PS1 com motor de som próprio.

## Correções

- O índice de música das ROM parecia parado enquanto uma ROM demorava a ser lida.
- O índice de música das ROM ignorava as ROM identificadas apenas por uma correspondência com um DAT.
- Uma verificação deixava sem plataforma as ROM japonesas de Super Famicom com o título interno em katakana.
- A página de um título repetia a mesma música para cada cópia do jogo encontrada.
- A indexação da música das ROM nunca terminava em algumas imagens de disco.
- Ao converter dumps de SNES numa cópia, um ficheiro cujo tamanho e cabeçalho interno discordavam
  sobre o cabeçalho de copiadora era convertido na mesma.

**Análise**
- No Linux, uma análise ignorava entradas de cue, gdi e m3u com outras maiúsculas ou com «\» como
  separador, e tomava dois ficheiros que só diferem nas maiúsculas por uma única ROM.
- Analisar uma pasta ignorava o tipo e a plataforma definidos nas suas subpastas.
- As reanálises voltavam a ler jogos em pasta, separavam faixas e perdiam correspondências por
  número de série. Agrupar discos em pastas grandes é mais rápido.
- Os ficheiros que não são ROM (texto, imagens) passam a tornar-se jogos só depois de
  identificados.
- Um de crc32, md5 e sha1 tem agora de ficar ativo nos hashes da análise. Só com xxh3_128 perdiam-se
  as correspondências DAT.

**Cópia**
- Ao substituir, o ficheiro antigo era apagado antes de o novo ser escrito, uma cópia reescrevia
  listas de reprodução a manter e dois ficheiros que só diferem nas maiúsculas eram escritos num só.

**Junção de títulos**
- A junção juntava versões de um conjunto de versões, ou títulos comerciais com títulos sem
  licença. «Aceitar tudo» repunha junções desfeitas e voltavam ligações IGDB removidas pela
  auditoria.
- Uma correspondência Hasheous pelo outro nome de um jogo junto criava um título duplicado.

**Servidor e tarefas**
- Obter arte para um cliente podia chegar à rede interna do servidor. Foi também adicionado um
  limite de tamanho.
- Descarregar as caches SQLite «database» e «all» passa a exigir um administrador.
- Uma sincronização completa interrompida deixava uma tabela a meio, e depois de cancelar uma cópia
  de segurança a seguinte começava enquanto ainda se escrevia.
- As cópias para outra unidade e as sincronizações a partir de outro servidor esperavam por tarefas
  alheias, e depois de reiniciar a base de dados uma tarefa podia começar duas vezes.

**Outros**
- A música das ROM falhava com um set PSF2.
- Partes da consola web e do ecrã de definições ficavam em inglês.
- O MariaDB anterior ao 10.6 passa a mostrar um erro ao ligar.

# romexplorer 0.2.0 (alpha)

## Alterações

**IA na Revisão de fusões**
- «Perguntar ao Google» é agora um menu Ask. Abre o modo IA do Google, Claude, ChatGPT ou
  Perplexity no navegador com a pergunta já escrita.
- As respostas na app podem vir do Gemini, Claude, OpenAI ou Perplexity, cada um com a sua chave de
  API e modelo.
- As respostas passam a ser guardadas. «Perguntar de novo» obtém uma nova; as afixadas nunca são
  pedidas de novo.
- As respostas guardadas estão incluídas nas cópias de segurança da base de dados.

**Config › AI**
- Foi adicionado um separador de definições de IA.
- Mostra o número de respostas guardadas e o custo estimado deste mês e no total. As respostas
  podem ser exportadas e importadas em JSON.

**IA no separador Definitiva**
- A página de cada jogo pode perguntar a uma IA em que diferem as suas versões, para escolher a
  definitiva. As respostas recebem os mesmos níveis que as notas.
- Antes da primeira pergunta é mostrado um aviso de custo. Config › AI liga à página de limites de
  utilização de cada fornecedor.
- «Avaliar em lote com IA» pergunta por vários jogos numa só tarefa. Mostra primeiro o número de
  perguntas e o custo estimado, e salta os jogos já respondidos.
- Os jogos com uma versão nova lançada depois da avaliação são marcados «Versão nova desde a
  avaliação» e podem ser excluídos pelo filtro.

**Títulos: versões menores e grupos de versões**
- Foi adicionado «Ocultar versões menores».
- Foi adicionado «Uma de cada conjunto de versões».
- Foi adicionado um cartão «Versões» às páginas de título.

**Config › Avançado**
- Foi adicionado um separador que reúne as ações que podem danificar a base de dados ou perder
  ficheiros. Otimizar a DB passou para aqui a partir de Tarefas e dados › DB.

**Áudio das ROM**
- As páginas de título podem tocar a música das ROM (faixas de áudio CD de jogos em disco e
  arquivos de som de Nintendo DS e 3DS).
- Os critérios das faixas que tocam podem ser ajustados nas definições.
- Foi adicionado um botão «não tocar esta faixa» ao leitor da página de título.
- A música das ROM descodificada para tocar fica em cache na memória (Definições › Áudio, 256 MiB
  por predefinição).
- A normalização do leitor foi afinada.
- O leitor pode fazer transição cruzada (2000 ms por predefinição, 0 desliga).
- O silêncio no fim das faixas passa a ser cortado.
- A música das páginas de título passa a estar ligada por predefinição. No primeiro arranque, depois
  do idioma, a app pergunta se deve tocar música. Os títulos sem áudio próprio tocam uma faixa
  aleatória por predefinição.

**Revisão de fusões e Títulos combinados**
- Um lançamento num cartão pode ser arrastado para o título certo. Largá-lo em «+» cria um título
  novo.
- Os cartões aceites e rejeitados podem ser mostrados com o filtro de estado. Por predefinição
  ficam ocultos.
- As separações também são registadas em Editar › Títulos combinados e podem ser desfeitas aí.
  Aparecem também as feitas na Revisão de fusões antes desta atualização.

**Lançamentos combinados**
- Os discos de um jogo registados como lançamentos separados podem ser selecionados e juntos com
  «Combinar num só lançamento».

**Análise**
- Foi adicionado «Voltar a analisar este ficheiro» às páginas de ficheiro. Volta a ler o ficheiro
  com os outros ficheiros da ROM e associa logo o jogo ao seu lançamento.
- As páginas de ficheiro passam a mostrar o lançamento e o título ligados.

## Correções

- Os nomes com hífen dentro de uma palavra não correspondiam à grafia do IGDB, pelo que alguns
  jogos não encontravam correspondência no IGDB.
- As sincronizações IGDB de SNES e NES deixavam de fora jogos lançados só no Japão.
- Desligar o IGDB deixava no título os nomes alternativos do IGDB, sem forma de os apagar. Agora
  saem com a ligação e cada um pode ser apagado com ✕.
- As ligações IGDB removidas à mão voltavam a ser ligadas por «Atualizar metadados». Agora ficam
  desligadas até serem ligadas de novo à mão.
- Os subconjuntos do RetroAchievements (desafios, speedruns, etc.) entravam no título do jogo e
  podiam juntar versões diferentes num título.
- Desfazer uma fusão feita na Revisão de fusões não trazia de volta a sugestão.
- As sugestões de títulos já fundidos à mão ficavam na Revisão de fusões.
- Os jogos piratas e sem licença eram fundidos com títulos comerciais.
- Os cartões «outro jogo lá dentro» deixavam no título jogos piratas e sem licença de outras
  regiões.
- Os cartões «outro jogo lá dentro» tomavam por jogos diferentes os lançamentos com um número antes
  do nome («0743 - ...»).
- Tarefas como a auditoria de fusões passam a mostrar progresso detalhado.
- No Windows arm64 e no Linux arm64 não era possível converter imagens de Wii U, Xbox e Xbox 360,
  ler ficheiros PBP nem tocar a música das ROM.
- Os ficheiros listados num DAT ficavam sem ligação a nenhum lançamento nem título.

# romexplorer 0.1.8 (alpha)

## Alterações

**Média**
- Recomeçar com uma base de dados nova passa a reutilizar a arte que já está na pasta de média.

**Tarefas**
- Os detalhes da lista de tarefas foram ajustados.
- Foi adicionado um botão «Executar a seguir» às tarefas em espera.

**Correspondência por hash**
- A correspondência por hash só com o Hasheous passa a obter também dados e arte do IGDB.
- serial_match pode agora escolher por plataforma se usa os números de série dos nomes de ficheiro.

**Revisão de fusões**
- As respostas do Gemini e «Perguntar ao Google» passam a usar o idioma da interface.
- Um pedido falhado passa a mostrar o motivo.
- O Gemini passa a ser consultado mesmo quando a sua lista de modelos não pode ser lida.

**Definitiva e Crossplat**
- Passam a ser mostrados só os títulos com ID do IGDB.

## Correções

**Coleção**
- A pesquisa no separador Definitiva só encontrava o nome do título principal.
