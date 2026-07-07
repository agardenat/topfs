# topfs

Affichage temps réel des plus gros fichiers et répertoires d'un système de fichiers, sous forme d'arborescence. Scan local parallèle, ou distant sur HDFS / Azure Blob (`abfs`) via le client `hdfs` Kerberos-aware. Sortie optionnelle vers Slack.

Écrit en Rust, distribué en binaire statique (musl) pour Linux.

## Aperçu

```
             /data/logs
  12.4 GiB   ├── app/ (42 2026-07-01 14:22)
   8.1 GiB   │   ├── access.log.2026-06 (2026-06-30 23:59)
   4.2 GiB   │   └── error.log (2026-07-01 14:22)
   3.9 GiB   └── nginx/ (18 2026-07-01 09:10)
  done  1284 entries  total:  16.3 GiB
```

Les tailles sont colorées par ordre de grandeur (cyan < vert < jaune < magenta < rouge). Les répertoires du top-N affichent entre parenthèses leur nombre d'enfants directs et leur date de dernière modification.

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
| `--days` | `-d` | | Ne compte que les fichiers modifiés dans les N derniers jours ; les plus anciens sont exclus de l'accumulation. |
| `--slack` | | | Envoie le résultat à une URL de webhook Slack (désactive l'affichage temps réel). Sans valeur, écrit un format compatible Slack sur stdout. |
| `--message` | `-m` | | Message d'en-tête à inclure dans la sortie Slack. |
| `--help` | `-h` | | Affiche l'aide. |
| `--version` | `-V` | | Affiche la version. |

### Usage disque vs taille apparente

Par défaut, `topfs` compte l'usage disque réel (`blocks × 512`, comme `du`), ce qui reflète l'espace occupé y compris pour les fichiers creux. `--apparent-size` (`-a`) compte la taille logique du fichier (comme `du --apparent-size` ou `ls -l`).

### Chemins distants (HDFS / Azure)

Les chemins distants sont détectés par leurs préfixes `hdfs://`, `abfs://`, `abfss://` ou `///`. Le scan délègue à la commande `hdfs dfs -ls -R`, qui doit être présente dans le `PATH` et utilise le client Java Hadoop (compatible Kerberos). Les préfixes sont normalisés vers `hdfs:///...`.

Pour ces chemins, la date de modification provient directement de la sortie `hdfs`, et le filtre `--days` compare les dates au format `YYYY-MM-DD HH:MM`.

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

- Scan local parallèle via `jwalk` (un pool Rayon par cœur CPU), tailles accumulées dans une `DashMap` concurrente.
- Après le scan, l'arbre est élagué au top-N (plus les ancêtres nécessaires à l'affichage) puis enrichi avec le nombre d'enfants et la date de modification.
- L'affichage compacte les chaînes de répertoires à enfant unique (`a/b/c`) et tronque proprement les lignes trop longues pour le terminal.

## Licence

Apache-2.0. Voir [LICENSE](LICENSE).
