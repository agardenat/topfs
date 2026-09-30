# topfs

Affichage temps réel des plus gros fichiers et répertoires d'un système de fichiers, sous forme d'arborescence. Scan local parallèle, ou distant sur HDFS / Azure Blob (`abfs`) via le client `hdfs` Kerberos-aware. Sortie optionnelle vers Slack.

Écrit en Rust, distribué en binaire statique (musl) pour Linux.

## Aperçu

```
             /data/logs
  12.4 GiB   ├── app/ (1 284 files, 2026-07-01 14:22)
   8.1 GiB   │   ├── access.log.2026-06 (2026-06-30 23:59)
   4.2 GiB   │   └── error.log (2026-07-01 14:22)
   3.9 GiB   └── nginx/ (312 files, 2026-07-01 09:10)
  done  1284 entries  total:  16.3 GiB
```

Les tailles sont colorées par ordre de grandeur (cyan < vert < jaune < magenta < rouge). Les répertoires du top-N affichent entre parenthèses le nombre de fichiers qu'ils contiennent — **récursivement**, comme la taille — et leur date de dernière modification. Le compte apparaît dès le scan et se met à jour en continu.

## Installation

### Homebrew (macOS / Linux)

```bash
brew install agardenat/topfs/topfs
```

### Paquet Debian / Ubuntu

Récupérer le `.deb` de la dernière [release](https://github.com/agardenat/topfs/releases), puis :

```bash
sudo dpkg -i topfs_*_amd64.deb
```

### Paquet RPM (Fedora / RHEL)

Récupérer le `.rpm` de la dernière [release](https://github.com/agardenat/topfs/releases), puis :

```bash
sudo rpm -i topfs-*.x86_64.rpm
```

> Les binaires empaquetés sont liés statiquement (musl), sans dépendance runtime.

### Depuis les sources (Cargo)

```bash
git clone https://github.com/agardenat/topfs.git
cd topfs
cargo build --release
install -Dm755 target/release/topfs ~/.local/bin/topfs
```

Toolchain Rust stable récente (edition 2021) requise.

## Utilisation

```bash
topfs [OPTIONS] [PATH]
```

`PATH` par défaut : `.` (répertoire courant).

### Exemples

```bash
topfs                          # scan du répertoire courant, top 20
topfs -n 40 /var               # top 40 sous /var
topfs -a ~/Downloads           # taille apparente (au lieu de l'usage disque)
topfs -d 7 /data               # seulement les fichiers modifiés dans les 7 derniers jours
topfs --since 2026-01-01 /data # fichiers modifiés depuis le 1er janvier 2026
topfs --until 2025-01-01 /data # fichiers plus vieux que le 1er janvier 2025
topfs --since 2025-01-01 --until 2025-07-01 /data   # fenêtre entre deux dates
topfs --older-than 90d /data   # fichiers dont la dernière modification remonte à plus de 90 jours
topfs hdfs:///user/data        # scan HDFS via le client hdfs
topfs abfs://container@account/path   # scan Azure Blob Storage
```

Le scan est incrémental et l'affichage se rafraîchit en continu. `Ctrl-C` interrompt proprement (le curseur est restauré).

## Paramètres

| Option | Alias | Défaut | Description |
|--------|-------|--------|-------------|
| `[PATH]` | | `.` | Chemin à scanner : local, `hdfs://host:port/path`, ou `abfs://container@account/path`. |
| `--count` | `-n` | `20` | Nombre d'entrées à afficher dans le top. |
| `--refresh-ms` | `-r` | `100` | Intervalle de rafraîchissement de l'affichage, en millisecondes. |
| `--apparent-size` | `-a` | `false` | Utilise la taille apparente (`len`) au lieu de l'usage disque réel (`blocks × 512`). |
| `--days` | `-d` | | Ne compte que les fichiers modifiés dans les N derniers jours ; les plus anciens sont exclus de l'accumulation. Incompatible avec `--since`. |
| `--since` | `-S`, `--newer-than` | | Ne compte que les fichiers modifiés **à partir de** cet instant (borne incluse). |
| `--until` | `-U`, `--older-than` | | Ne compte que les fichiers modifiés **strictement avant** cet instant. |
| `--slack` | | | Envoie le résultat à une URL de webhook Slack (désactive l'affichage temps réel). Sans valeur, écrit un format compatible Slack sur stdout. |
| `--message` | `-m` | | Message d'en-tête à inclure dans la sortie Slack. |
| `--help` | `-h` | | Affiche l'aide. |
| `--version` | `-V` | | Affiche la version. |

### Filtres temporels

`--since` et `--until` acceptent deux formes de valeur :

| Forme | Exemples | Sens |
|-------|----------|------|
| Date absolue | `2026-01-01`, `"2026-01-01 08:30"`, `2026-01-01T08:30:00` | Instant précis, interprété en **UTC** (comme les dates affichées par `topfs`). Sans heure, minuit. |
| Âge relatif | `30m`, `12h`, `7d`, `2w` | `maintenant - durée` (`s`, `m`, `h`, `d`, `w`). |

Les deux options se combinent pour délimiter une fenêtre semi-ouverte `[since, until[` :

```bash
topfs --since 2025-01-01 --until 2025-07-01 /data
topfs --newer-than 2w --older-than 2d /var/log
```

Une fenêtre vide (`since >= until`) est rejetée. Le filtre actif est rappelé dans la ligne de statut et dans la sortie Slack.

Seuls les fichiers sont filtrés : les répertoires restent affichés, avec la taille **et le nombre de fichiers** cumulés sur les seuls fichiers retenus. Un répertoire de 10 000 fichiers dont 3 sont récents affiche donc `(3 files, ...)` sous `--since`.

### Usage disque vs taille apparente

Par défaut, `topfs` compte l'usage disque réel (`blocks × 512`, comme `du`), ce qui reflète l'espace occupé y compris pour les fichiers creux. `--apparent-size` (`-a`) compte la taille logique du fichier (comme `du --apparent-size` ou `ls -l`).

### Chemins distants (HDFS / Azure)

Les chemins distants sont détectés par leurs préfixes `hdfs://`, `abfs://`, `abfss://` ou `///`. Le scan délègue à la commande `hdfs dfs -ls -R`, qui doit être présente dans le `PATH` et utilise le client Java Hadoop (compatible Kerberos). Les préfixes sont normalisés vers `hdfs:///...`.

Pour ces chemins, la date de modification provient directement de la sortie `hdfs` au format `YYYY-MM-DD HH:MM` ; les filtres `--days`, `--since` et `--until` s'appliquent de la même façon, en comparant ces dates telles quelles (fuseau du cluster).

## Sortie Slack

Deux modes selon la valeur passée à `--slack` :

- **Avec URL** — envoie l'arborescence dans un bloc de code vers le webhook :

  ```bash
  topfs -n 15 --slack "https://hooks.slack.com/services/XXX/YYY/ZZZ" -m "Rapport disque nocturne" /data
  ```

- **Sans valeur** — écrit le format Slack (texte brut, sans couleurs ANSI) sur stdout, utile pour rediriger vers un autre outil :

  ```bash
  topfs --slack -m "Top fichiers" /var/log
  ```

En mode Slack, l'affichage temps réel est désactivé ; le scan s'exécute puis produit le rapport final.

## Fonctionnement

- Scan local parallèle via `jwalk` (un pool Rayon par cœur CPU). Seuls les répertoires sont conservés en mémoire (taille et nombre de fichiers agrégés dans une `DashMap` concurrente) ; pour les fichiers, seul le top-N est gardé. La mémoire dépend donc du nombre de répertoires, pas du nombre de fichiers.
- Après le scan, le top-N final est enrichi avec la date de modification.
- Pendant le scan, l'affichage temps réel est limité à la hauteur du terminal ; l'arbre complet est affiché à la fin.
- L'affichage compacte les chaînes de répertoires à enfant unique (`a/b/c`) et tronque proprement les lignes trop longues pour le terminal.

## Licence

Apache-2.0. Voir [LICENSE](LICENSE).
