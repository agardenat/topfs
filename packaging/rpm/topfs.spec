Name:           topfs
Version:        %{_topfs_version}
Release:        1%{?dist}
Summary:        Live top-N biggest filesystem entries with tree display

License:        Apache-2.0
URL:            https://github.com/agardenat/topfs
Source0:        topfs

%description
Interactive terminal tool that continuously reports the largest
filesystem entries as a live-updating tree.

%prep
# Prebuilt binary is consumed directly; nothing to unpack.

%install
install -Dm755 %{SOURCE0} %{buildroot}%{_bindir}/topfs

%files
%{_bindir}/topfs

%changelog
* Mon May 18 2026 Antoine Gardenat <agardenat@leisambro.net> - %{_topfs_version}-1
- Packaged release.
