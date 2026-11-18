*Ceci est une traduction des notes de version 0.2.1, 0.2.0 et 0.1.8. Les versions antérieures se
trouvent dans [release_notes.md](release_notes.md) (anglais), qui fait foi en cas de divergence.*

# romexplorer 0.3.0 (alpha)

Alpha : le schéma de la base de données peut changer d'une version à l'autre sans mise à niveau.
Faites une sauvegarde avant de remplacer une version.

## Changements

**Analyse**
- Ajout de « Analyse rapide (modifiés) » : elle lit aussi les fichiers dont la taille ou la date a changé.
- Les hash de l'analyse se règlent séparément pour les fichiers ordinaires, les fichiers d'archive et le contenu des archives. Les fichiers d'archive utilisent sha1 et xxh3_128 par défaut.

**Copie**
- Une copie relancée vérifie plusieurs fichiers déjà présents à la destination à la fois.

**Listes**
- Les vues en cartes avec beaucoup d'entrées s'ouvrent et défilent plus vite.

**Progression**
- Chaque tâche indique ce qu'elle fait, le total et où elle en est, y compris pendant le parcours des dossiers, les vérifications et les étapes de base de données.
- Les tâches en plusieurs étapes listent leurs étapes.
- Les tâches ont un bouton « détails ».
- Les tâches en cours indiquent le temps écoulé et le temps restant.
- Les gros téléchargements et les vérifications de fichiers indiquent la quantité déjà lue.
- Vider le catalogue et importer un fichier de collections s'exécutent comme des tâches.
- L'application de bureau indique ce qu'elle fait pendant son démarrage.

**Audio**
- Plusieurs pistes peuvent être converties en même temps pour la lecture.

## Corrections

- La progression de l'analyse audio s'arrêtait pendant la mesure du volume sonore.
- La progression de l'index de musique des ROM n'avançait que toutes les 500 ROM.

# romexplorer 0.2.2 (alpha)

Alpha : le schéma de la base de données peut changer d'une version à l'autre sans mise à niveau.
Faites une sauvegarde avant de remplacer une version.

## Changements

**Audio des ROM**
- L'index de musique des ROM peut lire plusieurs ROM à la fois (4 par défaut, réglable dans les paramètres audio).
- L'index de musique des ROM ne lit que la copie la mieux classée de chaque titre, et les autres seulement si elle n'a pas de musique.
- Pour les jeux sur plusieurs disques, les fichiers de musique déjà présents sur un disque précédent ne sont pas relus.
- Les jeux sur cartouche dont l'application connaît déjà la musique la jouent dès l'ouverture de la page du titre, sans attendre l'index.
- Ajout de « Effacer l'index audio des ROM » (toutes ou par plateforme).

# romexplorer 0.2.1 (alpha)

Alpha : le schéma de la base de données peut changer d'une version à l'autre sans mise à niveau.
Faites une sauvegarde avant de remplacer une version.

## Changements

**3DS**
- Les ROM 3DS peuvent être converties vers d'autres formats.
- Aucune clé n'est fournie. Placez aes_keys.txt (et seeddb.bin si un titre en a besoin) à côté des
  fichiers, à côté de romexplorer ou dans son dossier assets.
- Correspondance par numéro de série pour les ROM 3DS.

**Audio des ROM**
- L'ouverture de la page d'un titre indexe maintenant en premier la musique des ROM de ce jeu.
- L'index de musique des ROM lit maintenant les plus petites ROM en premier et ignore les ROM non
  prises en charge.
- Les jeux PS1 dotés de leur propre moteur sonore sont pris en charge.

## Corrections

- L'index de musique des ROM semblait bloqué pendant la lecture d'une ROM longue à lire.
- L'index de musique des ROM ignorait les ROM identifiées uniquement par une correspondance DAT.
- Une analyse laissait sans plateforme les ROM japonaises de Super Famicom dont le titre interne est en katakana.
- La page d'un titre répétait la même musique pour chaque copie du jeu trouvée.
- L'indexation de la musique des ROM ne se terminait jamais sur certaines images disque.
- Lors de la conversion de dumps SNES par une copie, un fichier dont la taille et l'en-tête interne
  se contredisaient sur l'en-tête de copieur était converti quand même.

**Analyse**
- Sous Linux, une analyse ignorait les entrées cue, gdi et m3u écrites avec une autre casse ou avec
  « \ » comme séparateur, et prenait deux fichiers ne différant que par la casse pour une seule ROM.
- L'analyse d'un dossier ignorait le type et la plateforme définis sur ses sous-dossiers.
- Les réanalyses relisaient les jeux en dossier, séparaient les pistes et perdaient les
  correspondances par numéro de série. Le regroupement des disques dans les grands dossiers est
  plus rapide.
- Les fichiers qui ne sont pas des ROM (texte, images) ne deviennent maintenant des jeux qu'une fois
  identifiés.
- L'un de crc32, md5 et sha1 doit maintenant rester actif dans les hashs de l'analyse. Avec
  xxh3_128 seul, les correspondances DAT étaient perdues.

**Copie**
- L'écrasement supprimait l'ancien fichier avant d'écrire le nouveau, une copie réécrivait des
  listes de lecture à conserver, et deux fichiers ne différant que par la casse étaient écrits en un.

**Fusion des titres**
- La fusion réunissait des versions d'un groupe de versions, ou des titres commerciaux avec des
  titres sans licence. « Tout accepter » rétablissait des fusions défaites, et des liens IGDB retirés
  par l'audit revenaient.
- Une correspondance Hasheous sur l'autre nom d'un jeu fusionné créait un titre en double.

**Serveur et tâches**
- La récupération d'images pour un client pouvait atteindre le réseau interne du serveur. Une
  limite de taille a aussi été ajoutée.
- Le téléchargement des caches SQLite « database » et « all » nécessite maintenant un
  administrateur.
- Une synchronisation complète interrompue laissait une table à moitié remplie, et après
  l'annulation d'une sauvegarde la suivante démarrait pendant l'écriture.
- Les copies vers un autre disque et les synchronisations depuis un autre serveur attendaient des
  tâches sans rapport, et une tâche pouvait démarrer deux fois après un redémarrage de la base.

**Divers**
- La musique des ROM plantait sur un set PSF2.
- Des parties de la console web et de l'écran des paramètres restaient en anglais.
- MariaDB antérieur à 10.6 affiche maintenant une erreur à la connexion.

# romexplorer 0.2.0 (alpha)

## Changements

**L'IA dans Revue des fusions**
- « Demander à Google » est maintenant un menu Ask. Il ouvre le mode IA de Google, Claude, ChatGPT
  ou Perplexity dans le navigateur avec la question déjà saisie.
- Les réponses dans l'application peuvent venir de Gemini, Claude, OpenAI ou Perplexity, chacun
  avec sa clé d'API et son modèle.
- Les réponses sont maintenant enregistrées. « Redemander » en obtient une nouvelle ; les réponses
  épinglées ne sont jamais redemandées.
- Les réponses enregistrées sont incluses dans les sauvegardes de la base de données.

**Config › AI**
- Un onglet de paramètres de l'IA a été ajouté.
- Il affiche le nombre de réponses enregistrées et le coût estimé du mois et au total. Les
  réponses peuvent être exportées et importées en JSON.

**L'IA dans l'onglet Définitive**
- La page de chaque jeu peut demander à une IA en quoi ses versions diffèrent, pour choisir la
  définitive. Les réponses reçoivent les mêmes rangs que les notes.
- Un avertissement de coût s'affiche avant la première question. Config › AI renvoie à la page des
  limites d'utilisation de chaque fournisseur.
- « Évaluer en lot avec l'IA » interroge sur plusieurs jeux en une seule tâche. Il affiche d'abord
  le nombre de questions et le coût estimé, et ignore les jeux déjà traités.
- Les jeux ayant une nouvelle version sortie après leur évaluation sont marqués « Nouvelle version
  depuis l'évaluation » et peuvent être exclus par le filtre.

**Titres : versions mineures et groupes de versions**
- « Masquer les versions mineures » a été ajouté.
- « Une version de chaque ensemble » a été ajouté.
- Une carte « Versions » a été ajoutée aux pages de titre.

**Config › Avancé**
- Un onglet regroupe les actions pouvant endommager la base de données ou faire perdre des
  fichiers. L'optimisation de la DB y a été déplacée depuis Tâches et données › DB.

**Audio des ROM**
- Les pages de titre peuvent lire la musique contenue dans les ROM (pistes audio CD des jeux sur
  disque et archives sonores Nintendo DS et 3DS).
- Les critères des pistes lues se règlent dans les paramètres.
- Un bouton « ne pas jouer cette piste » a été ajouté au lecteur de la page de titre.
- La musique des ROM décodée pour la lecture est mise en cache en mémoire (Paramètres › Audio,
  256 Mio par défaut).
- La normalisation du lecteur a été ajustée.
- Le lecteur peut faire un fondu enchaîné (2000 ms par défaut, 0 le désactive).
- Le silence en fin de piste est maintenant coupé.
- La musique des pages de titre est maintenant activée par défaut. Au premier lancement, après la
  langue, l'application demande s'il faut jouer de la musique. Les titres sans audio propre jouent
  une piste au hasard par défaut.

**Revue des fusions et Titres fusionnés**
- Une sortie d'une carte peut être glissée sur le bon titre. La déposer sur « + » crée un nouveau
  titre.
- Les cartes acceptées et rejetées peuvent être affichées avec le filtre d'état. Elles sont masquées
  par défaut.
- Les séparations sont aussi consignées dans Modifier › Titres fusionnés et peuvent y être
  annulées. Celles faites dans la Revue des fusions avant cette mise à jour y figurent aussi.

**Sorties regroupées**
- Les disques d'un même jeu enregistrés comme sorties séparées peuvent être sélectionnés et réunis
  avec « Regrouper en une seule sortie ».

**Analyse**
- « Réanalyser ce fichier » a été ajouté aux pages de fichier. Il relit le fichier avec les autres
  fichiers de la ROM et associe aussitôt le jeu à sa sortie.
- Les pages de fichier affichent maintenant la sortie et le titre liés.

## Corrections

- Les noms avec un trait d'union au milieu d'un mot ne correspondaient pas à l'orthographe d'IGDB,
  si bien que certains jeux ne trouvaient pas de correspondance IGDB.
- Les synchronisations IGDB SNES et NES omettaient les jeux sortis uniquement au Japon.
- Délier IGDB laissait les noms alternatifs d'IGDB sur le titre, sans moyen de les supprimer. Ils
  partent maintenant avec le lien, et chacun peut être supprimé avec ✕.
- Les liens IGDB retirés à la main étaient refaits par « Mettre à jour les métadonnées ». Ils restent
  maintenant déliés jusqu'à un nouveau lien manuel.
- Les sous-ensembles RetroAchievements (défis, speedruns, etc.) entraient dans le titre du jeu et
  pouvaient réunir des versions différentes en un titre.
- Annuler une fusion faite dans la Revue des fusions ne faisait pas revenir la suggestion.
- Les suggestions de titres déjà fusionnés à la main restaient dans la Revue des fusions.
- Les jeux pirates et sans licence étaient fusionnés avec des titres commerciaux.
- Les cartes « un autre jeu à l'intérieur » laissaient dans le titre des jeux pirates et sans
  licence d'autres régions.
- Les cartes « un autre jeu à l'intérieur » prenaient pour des jeux différents les sorties ayant un
  numéro devant le nom (« 0743 - ... »).
- Les tâches comme l'audit des fusions affichent maintenant une progression détaillée.
- Sous Windows arm64 et Linux arm64, les images Wii U, Xbox et Xbox 360 ne pouvaient pas être
  converties, les fichiers PBP lus, ni la musique des ROM jouée.
- Des fichiers listés dans un DAT restaient liés à aucune sortie ni aucun titre.

# romexplorer 0.1.8 (alpha)

## Changements

**Médias**
- Repartir avec une nouvelle base de données réutilise maintenant les images déjà présentes dans le
  dossier des médias.

**Tâches**
- Les détails de la liste des tâches ont été ajustés.
- Un bouton « Exécuter ensuite » a été ajouté aux tâches en attente.

**Correspondance par hash**
- La correspondance par hash avec Hasheous seul récupère maintenant aussi les données et images
  IGDB.
- serial_match peut maintenant choisir par plateforme d'utiliser ou non les numéros de série des
  noms de fichier.

**Revue des fusions**
- Les réponses de Gemini et « Demander à Google » utilisent maintenant la langue de l'interface.
- Une requête échouée affiche maintenant la raison.
- Gemini est maintenant interrogé même si sa liste de modèles ne peut pas être lue.

**Définitive et Crossplat**
- Seuls les titres ayant un ID IGDB sont maintenant affichés.

## Corrections

**Collection**
- La recherche dans l'onglet Définitive ne trouvait que le nom du titre principal.
