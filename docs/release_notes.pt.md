*Esta é uma tradução das notas das versões 0.2.0 e 0.1.8. As versões anteriores estão em
[release_notes.md](release_notes.md) (inglês), que prevalece em caso de diferença.*

# romexplorer 0.2.0 (alpha)

Alfa: o esquema da base de dados pode mudar entre versões sem forma de atualização. Faça uma cópia
de segurança antes de substituir uma versão.

## Alterações

**IA na Revisão de fusões**
- «Perguntar ao Google» passou a ser um menu Ask: o modo IA do Google, o Claude, o ChatGPT e o
  Perplexity abrem no navegador com a pergunta já escrita.
- A resposta na aplicação pode vir do Gemini, do Claude, da OpenAI ou do Perplexity, cada um com a
  sua chave de API e o seu modelo.
- As respostas ficam guardadas. «Perguntar de novo» obtém uma nova; as respostas afixadas não são
  pedidas de novo.
- As respostas guardadas fazem parte da cópia de segurança da base de dados.

**Config › AI**
- Um separador novo para as definições da IA.
- Mostra quantas respostas estão guardadas e o seu custo aproximado este mês e no total. As
  respostas podem ser exportadas e importadas em JSON.

**IA no separador Definitiva**
- A página de cada jogo pode perguntar à IA qual a versão com mais conteúdo e com melhores
  gráficos, som e comodidade de jogo. A resposta traz uma classificação, como as notas revistas.
- Uma resposta obtida por uma ligação do navegador pode ser colada, ou guardada a partir da área de
  transferência com um clique.
- A primeira pergunta mostra um aviso de custo. Config › AI tem ligações para a página de limites
  de gastos de cada fornecedor.
- «Avaliar em lote com IA» pergunta sobre vários jogos numa só tarefa: os jogos escolhidos no
  separador Definitiva, ou as linhas escolhidas de um conjunto ou o conjunto inteiro. Primeiro
  mostra o número de perguntas e um custo aproximado, salta os jogos já respondidos e pode ser
  agendada para um conjunto.
- Uma classificação dada antes de sair uma versão mais recente do jogo fica marcada «Nova versão
  desde a avaliação», e o filtro pode excluir essas classificações.

**Títulos: versões menores e grupos de versões**
- «Ocultar versões menores» oculta um jogo quando existe uma edição mais completa na mesma plataforma e
  região (International, Director's Cut, Final Mix e semelhantes; uma Easy Type cede ao jogo
  normal). Em Títulos, Lançamentos e na lista de lançamentos de um título.
- «Uma de cada conjunto de versões» mostra só uma versão dos jogos lançados em várias versões (Pokémon,
  Yo-kai Watch e outros): a que tem, senão a que escolheu, senão uma ao acaso, com 🎲 para ver
  outra. As versões de um mesmo grupo já não formam uma só família no separador Definitiva.
- O novo cartão Versões na página de um título mostra ambos e permite corrigi-los.

**Config › Avançado**
- Um separador novo para as operações que podem danificar a base de dados ou fazer perder
  ficheiros. Otimizar a DB passou para aqui a partir de Tarefas e dados › DB.

**Áudio das ROM**
- As páginas de título podem tocar a música que está dentro das suas ROM (só plataformas e tipos de
  suporte compatíveis).
- Nas definições escolhe-se que faixas tocam.
- São suportados os formatos de áudio originais (NSF, SPC, GBS, VGM, HES, KSS, AY, SAP e SGC
  da Game Gear, soltos ou em zip).
- Uma biblioteca de áudio original: indique em Definições › Áudio as pastas onde guarda os conjuntos, e
  «Indexar o áudio das ROM» associa cada conjunto às suas ROM pelo conteúdo. O mesmo conjunto toca em todas
  as regiões do jogo, seja qual for o nome.
- O leitor das páginas de título tem um botão «não tocar esta faixa». As faixas ocultas aparecem
  por baixo da lista de reprodução: um clique toca uma e Restaurar devolve-a.
- A música das ROM descodificada para tocar fica em cache na memória (Definições › Áudio, 256 MiB
  por predefinição). Quando enche, as faixas passam para a cache de áudio em disco, que usa a pasta
  da cache de nível 1 se estiver definida.
- A normalização do leitor foi ajustada.
- O leitor pode fazer transição cruzada (2000 ms por predefinição, 0 para desligar).
- O silêncio no fim de uma faixa é cortado: as faixas das ROM são aparadas, e nos ficheiros de
  áudio a análise de áudio mede onde o som acaba (volte a executá-la para os ficheiros analisados
  antes).
- A música das páginas de título está ligada por predefinição, e o primeiro arranque pergunta se a
  quer, logo a seguir ao idioma. Um título sem áudio próprio toca faixas ao acaso por predefinição.
- Os ficheiros PSF e PSF2 usam uma BIOS da PS2 da sua coleção, e são saltados sem ela.

**Revisão de fusões e Títulos combinados**
- Os lançamentos de um cartão podem ser arrastados para o título a que pertencem, ou para «+» para
  um título novo com o nome do primeiro. Tocar num lançamento e depois num título também funciona.
- Os cartões já aceites ou recusados podem voltar a ser mostrados com o novo filtro de estado.
  Estão ocultos por predefinição.
- As separações passam a aparecer em Editar › Títulos combinados e podem ser desfeitas aí. As
  feitas na Revisão de fusões antes desta atualização são acrescentadas à lista.

## Correções

- Os subsets do RetroAchievements (desafio, speedrun e afins) entravam no título do seu jogo e podiam juntar duas versões de um jogo num só título. Cada subset é agora um título próprio; a auditoria de fusões separa os que já lá estavam.
- Desfazer uma fusão aceite na revisão de fusões não repunha a sugestão. Volta agora a ficar por rever.
- Jogos piratas e sem licença eram fundidos nos títulos de jogos comerciais.
- O cartão «outro jogo lá dentro» deixava no título jogos piratas e sem licença de outras regiões.
- O cartão «outro jogo lá dentro» tomava por outro jogo um lançamento com um número antes do nome
  («0743 - ...»).
- Tarefas como a auditoria de fusões mostravam pouco do seu progresso. Agora mostram-no em
  detalhe.

# romexplorer 0.1.8 (alpha)

Alfa: o esquema da base de dados pode mudar entre versões sem forma de atualização. Faça uma cópia
de segurança antes de substituir uma versão.

## Alterações

**Média**
- Ao recomeçar com uma base de dados nova, as imagens que já estão na pasta de média são usadas em
  vez de serem descarregadas de novo.

**Tarefas**
- As colunas de detalhe da lista de tarefas foram ajustadas.
- Uma tarefa em espera tem um botão «Executar a seguir», também para uma seleção: começa assim que termina
  a tarefa em curso na sua fila. As outras filas não são afetadas.

**Correspondência por hash**
- Uma correspondência por hash só com o Hasheous ativo também obtém os dados do IGDB e as imagens
  do que encontrou, como com o PlayMatch.
- A definição serial_match escolhe por plataforma se o número de série no nome do ficheiro conta.
  Uma plataforma deixada como está segue a caixa do nome do ficheiro.

**Revisão de fusões**
- A resposta do Gemini e «Perguntar ao Google» chegam no idioma da interface.
- Quando o pedido falha, a mensagem diz porquê: um certificado ou um tempo esgotado, o modelo e a
  quota esgotada. Uma chave sem quota gratuita para o modelo indica-o.
- Se a lista de modelos não puder ser lida, o Gemini é consultado na mesma; num erro de quota,
  tenta-se uma vez o modelo Flash seguinte.

**Definitiva e Crossplat**
- Só são listados os títulos com um ID do IGDB. Os títulos da Wii U com o nome do seu ID de título
  deixam de aparecer em Definitiva.

## Correções

**Coleção**
- A pesquisa do separador Definitiva só encontrava o nome do título principal. Agora encontra uma
  família pelo nome de qualquer versão, incluindo os nomes localizados e alternativos.
