*Ceci est une traduction des notes de version 0.2.0 et 0.1.8. Les versions antérieures se
trouvent dans [release_notes.md](release_notes.md) (anglais), qui fait foi en cas de divergence.*

# romexplorer 0.2.0 (alpha)

Alpha : le schéma de la base de données peut changer d'une version à l'autre sans mise à niveau.
Faites une sauvegarde avant de remplacer une version.

## Changements

**L'IA dans Revue des fusions**
- « Demander à Google » devient un menu Ask : le mode IA de Google, Claude, ChatGPT et Perplexity
  s'ouvrent dans le navigateur avec la question déjà saisie.
- La réponse dans l'application peut venir de Gemini, Claude, OpenAI ou Perplexity, chacun avec sa
  clé d'API et son modèle.
- Les réponses sont enregistrées. « Redemander » en obtient une nouvelle ; les réponses épinglées ne
  sont pas redemandées.
- Les réponses enregistrées font partie de la sauvegarde de la base de données.

**Config › AI**
- Un nouvel onglet pour les réglages de l'IA.
- Affiche le nombre de réponses enregistrées et leur coût approximatif ce mois-ci et au total. Les
  réponses peuvent être exportées et importées en JSON.

**L'IA dans l'onglet Définitive**
- La page de chaque jeu peut demander à l'IA quelle version a le plus de contenu et les meilleurs
  graphismes, son et confort de jeu. La réponse porte un rang, comme les notes relues.
- Une réponse obtenue par un lien du navigateur peut être collée, ou enregistrée depuis le
  presse-papiers en un clic.
- La première question affiche un avertissement sur le coût. Config › AI renvoie vers la page des
  limites de dépense de chaque fournisseur.
- « Évaluer en lot avec l'IA » interroge sur plusieurs jeux en une seule tâche : les jeux choisis
  dans l'onglet Définitive, ou les lignes choisies d'un set ou le set entier. Le nombre de questions
  et un coût approximatif s'affichent d'abord, les jeux déjà évalués sont sautés, et un set peut être
  planifié.
- Un rang donné avant la sortie d'une version plus récente du jeu est marqué « Nouvelle version
  depuis l'évaluation », et le filtre peut écarter ces rangs.

**Titres : versions mineures et groupes de versions**
- « Masquer les versions mineures » masque un jeu quand une édition plus complète existe sur la même
  plateforme et dans la même région (International, Director's Cut, Final Mix, etc. ; une Easy Type
  laisse la place au jeu normal). Dans Titres, Sorties et la liste des sorties d'un titre.
- « Une version de chaque ensemble » n'affiche qu'une version des jeux sortis en plusieurs versions
  (Pokémon, Yo-kai Watch, etc.) : celle que vous possédez, sinon votre choix, sinon une au hasard,
  avec 🎲 pour en voir une autre. Les versions d'un même groupe ne forment plus une seule famille
  dans l'onglet Définitive.
- La nouvelle carte Versions de la page d'un titre montre les deux et permet de les corriger.

**Config › Avancé**
- Un nouvel onglet pour les opérations qui peuvent endommager la base de données ou faire perdre
  des fichiers. L'optimisation de la DB y a été déplacée depuis Tâches et données › DB.

**Audio des ROM**
- Les pages de titre peuvent jouer la musique contenue dans vos ROM (plateformes et types de
  support pris en charge uniquement).
- Les réglages permettent de choisir quelles pistes sont jouées.
- Les formats audio d'origine (NSF, SPC, GBS, VGM, HES, KSS, AY, SAP et SGC de la
  Game Gear, seuls ou en zip) sont pris en charge.
- Une bibliothèque audio d'origine : indiquez dans Paramètres › Audio les dossiers où vous gardez vos sets,
  et « Indexer l'audio des ROM » associe chaque set à vos ROM par leur contenu. Un même set joue pour toutes
  les régions du jeu, quel que soit son nom.
- Le lecteur des pages de titre a un bouton « ne pas jouer cette piste ». Les pistes masquées sont
  listées sous la liste de lecture : un clic en joue une, Restaurer la remet.
- La musique des ROM décodée pour la lecture est mise en cache en mémoire (Paramètres › Audio,
  256 Mio par défaut). Au-delà, les pistes passent dans le cache audio sur disque, qui utilise le
  dossier du cache de niveau 1 s'il est défini.
- La normalisation du lecteur a été ajustée.
- Le lecteur peut faire un fondu enchaîné (2000 ms par défaut, 0 pour le désactiver).
- Le silence à la fin d'une piste est coupé : les pistes des ROM sont raccourcies, et pour les
  fichiers audio, le scan audio mesure où le son s'arrête (relancez-le pour les fichiers déjà
  scannés).
- La musique des pages de titre est activée par défaut, et le premier démarrage demande si vous la
  voulez, juste après la langue. Un titre sans audio propre joue des pistes au hasard par défaut.
- Les fichiers PSF et PSF2 utilisent un BIOS PS2 de votre collection, et sont sautés sans lui.

**Revue des fusions et Titres fusionnés**
- Les sorties d'une carte peuvent être glissées vers le titre auquel elles appartiennent, ou sur
  « + » pour un nouveau titre nommé d'après la première. Toucher une sortie puis un titre fonctionne
  aussi.
- Les cartes déjà acceptées ou refusées peuvent être réaffichées avec le nouveau filtre d'état.
  Elles sont masquées par défaut.
- Les séparations sont désormais listées dans Édition › Titres fusionnés et peuvent y être
  annulées. Celles faites depuis Revue des fusions avant cette mise à jour sont ajoutées à la liste.

**Sorties regroupées**
- Les disques d’un même jeu listés séparément (ceux d’un pack, un disque bonus) peuvent être réunis avec « Regrouper en une seule sortie ». Une seule sortie est listée pour tous, et la copier copie tous les disques. La page de la sortie permet d’en séparer un.

## Corrections

- Un titre gardait les titres alternatifs d’IGDB après le retrait de son lien IGDB, sans pouvoir les supprimer. Ils partent désormais avec le lien, et chaque titre alternatif peut être retiré avec ✕.
- « Mettre à jour les métadonnées » reliait de nouveau à IGDB un titre dont le lien avait été retiré à la main. Il reste désormais sans lien jusqu’à ce que vous le reliiez vous-même.
- Les subsets RetroAchievements (défi, speedrun, etc.) allaient dans le titre de leur jeu et pouvaient réunir deux versions d'un jeu en un titre. Chaque subset est désormais un titre à part ; l'audit des fusions sépare ceux déjà présents.
- Annuler une fusion acceptée dans la revue des fusions ne remettait pas la suggestion. Elle est de nouveau à examiner.
- Une suggestion de fusion restait dans la revue des fusions après la fusion manuelle de ses titres.
- Des jeux pirates et sans licence étaient fusionnés dans les titres de jeux commerciaux.
- La carte « un autre jeu à l'intérieur » laissait dans le titre des jeux pirates et sans licence
  d'autres régions.
- La carte « un autre jeu à l'intérieur » prenait une sortie avec un numéro devant son nom
  (« 0743 - ... ») pour un autre jeu.
- Des tâches comme l'audit des fusions montraient peu de leur progression. Elles la montrent
  maintenant en détail.

# romexplorer 0.1.8 (alpha)

Alpha : le schéma de la base de données peut changer d'une version à l'autre sans mise à niveau.
Faites une sauvegarde avant de remplacer une version.

## Changements

**Médias**
- Après un nouveau départ avec une nouvelle base de données, les visuels déjà présents dans le
  dossier des médias sont réutilisés au lieu d'être téléchargés à nouveau.

**Tâches**
- Les colonnes de détail de la liste des tâches ont été ajustées.
- Une tâche en attente a un bouton « Exécuter ensuite », aussi pour une sélection : elle démarre dès que la
  tâche en cours dans sa file est terminée. Les autres files ne sont pas touchées.

**Correspondance par hash**
- Une correspondance par hash avec seulement Hasheous activé récupère aussi les données IGDB et les
  visuels de ce qu'elle a trouvé, comme avec PlayMatch.
- Le réglage serial_match choisit par plateforme si le numéro de série dans le nom de fichier
  compte. Une plateforme laissée telle quelle suit la case du nom de fichier.

**Revue des fusions**
- La réponse de Gemini et « Demander à Google » arrivent dans la langue de l'interface.
- Quand la requête échoue, le message dit pourquoi : un certificat ou un délai dépassé, le modèle,
  et le quota épuisé. Une clé sans quota gratuit pour le modèle le signale.
- Si la liste des modèles ne peut pas être lue, Gemini est quand même interrogé ; en cas d'erreur
  de quota, le modèle Flash suivant est essayé une fois.

**Définitive et Crossplat**
- Seuls les titres avec un identifiant IGDB sont listés. Les titres Wii U nommés par leur
  identifiant de titre n'apparaissent plus dans Définitive.

## Corrections

**Collection**
- La recherche de l'onglet Définitive ne trouvait que le nom du titre de tête. Elle trouve
  maintenant une famille par le nom de n'importe quelle version, noms localisés et alternatifs
  compris.
