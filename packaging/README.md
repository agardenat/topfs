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
