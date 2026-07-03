# Packaging

Tout se déroule dans le dépôt. Les artefacts sont écrits dans `packaging/dist/`.

## Tout d'un coup

```bash
./packaging/build-all.sh
```

Construit ce que l'hôte sait faire (deb si `dpkg-deb`, rpm si `rpmbuild`) et
met à jour la formule Homebrew.

## .deb

```bash
./packaging/deb/build-deb.sh
```

Sortie : `packaging/dist/topfs_<version>_<arch>.deb`
Prérequis : `dpkg-deb` (paquet `dpkg`), cible Rust `x86_64-unknown-linux-musl`
(ajoutée automatiquement par le script).

Le binaire empaqueté est lié **statiquement** (musl, `static-pie`) : aucune
dépendance glibc, installable sur n'importe quelle distro x86_64.

## .rpm

```bash
./packaging/rpm/build-rpm.sh
```

Sortie : `packaging/dist/topfs-<version>-1.<arch>.rpm`
Prérequis : `rpmbuild` (`rpm-build` / `rpmdevtools`), cible
`x86_64-unknown-linux-musl`. Empaquette le binaire statique musl via
`packaging/rpm/topfs.spec` (aucune dépendance glibc dans le rpm).

## Homebrew (Linux et macOS)

```bash
./packaging/brew/update-formula.sh
```

Synchronise `packaging/brew/topfs.rb` (url du tag + sha256) avec la version
de `Cargo.toml`. La formule compile depuis les sources (`cargo install`).

Installation locale via tap :

```bash
brew install --build-from-source ./packaging/brew/topfs.rb
# ou la branche main :
brew install --HEAD ./packaging/brew/topfs.rb
```

Note : le sha256 calculé correspond au tarball `git archive`. Après avoir
poussé le tag `vX.Y.Z` sur GitHub, le tarball de release peut avoir un sha
différent — relancer le script ou recalculer si besoin.

## Version

Tous les scripts lisent la version depuis `Cargo.toml`. Bumper la version
là, puis relancer.

## Release GitHub (automatique)

Le workflow [`.github/workflows/release.yml`](../.github/workflows/release.yml)
se déclenche sur un tag `v*` et :

1. vérifie que le tag correspond à la version de `Cargo.toml` ;
2. construit le `.deb` et le `.rpm` (musl statique) ;
3. crée la GitHub Release et y attache les deux paquets ;
4. calcule le sha256 du tarball de release GitHub, patche la formule
   (`packaging/brew/topfs.rb` comme gabarit) et pousse `Formula/topfs.rb`
   dans le dépôt tap `homebrew-topfs`.

Publier une version :

```bash
git tag v1.0.0
git push origin v1.0.0
```

### Setup one-time du tap Homebrew

1. Créer un dépôt GitHub **public** nommé `homebrew-topfs` sous le même
   propriétaire (`agardenat/homebrew-topfs`). Peut rester vide : le workflow
   crée `Formula/topfs.rb` à la première release.
2. Générer un PAT (classic `repo`, ou fine-grained avec accès *Contents:
   write* sur `homebrew-topfs`).
3. L'ajouter comme secret du dépôt `topfs` sous le nom `TAP_GITHUB_TOKEN`
   (Settings → Secrets and variables → Actions).

Ensuite, côté utilisateur :

```bash
brew tap agardenat/topfs
brew install topfs
```

Le script local [`brew/update-formula.sh`](brew/update-formula.sh) reste utile
pour tester la formule hors CI ; en release, le workflow gère l'url + le
sha256 automatiquement.
