Name:           model-assistant
Version:        0.2.0
Release:        1%{?dist}
Summary:        GNOME desktop application for launching local AI model runtimes
URL:            https://github.com/fxzxmicah/model-assistant
License:        MIT

Source0:        %{url}/archive/refs/tags/%{version}.tar.gz#/%{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust-packaging
BuildRequires:  desktop-file-utils
BuildRequires:  appstream
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)

Requires(post): dconf
Requires(postun): dconf

%description
Model Assistant is a GNOME desktop application for launching local AI model
runtimes from a MODELS_ROOT layout. It validates the runtime environment,
launches model processes inside a runner rootfs, and provides per-model
console pages for output and interactive input.

%generate_buildrequires
%cargo_generate_buildrequires

%prep
%autosetup -p1
%cargo_prep

%build
%cargo_build

%install
install -Dpm0755 target/rpm/model-assistant %{buildroot}%{_bindir}/model-assistant
install -Dpm0755 target/rpm/runner-keeper %{buildroot}%{_libexecdir}/%{name}/runner-keeper

install -Dpm0644 \
    data/org.gnome.modelassistant.metainfo.xml \
    %{buildroot}%{_metainfodir}/org.gnome.modelassistant.metainfo.xml

install -Dpm0644 \
    data/icons/hicolor/scalable/apps/org.gnome.modelassistant.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/org.gnome.modelassistant.svg

install -Dpm0644 \
    data/dconf/db/distro.d/00-model-assistant-shortcuts \
    %{buildroot}%{_sysconfdir}/dconf/db/distro.d/00-model-assistant-shortcuts

install -Dpm0644 \
    data/dconf/db/distro.d/locks/00-model-assistant-shortcuts \
    %{buildroot}%{_sysconfdir}/dconf/db/distro.d/locks/00-model-assistant-shortcuts

desktop-file-install \
    --dir=%{buildroot}%{_datadir}/applications \
    data/org.gnome.modelassistant.desktop

install -d %{buildroot}%{_datadir}/dbus-1/services
sed 's#@bindir@#%{_bindir}#g' \
    data/org.gnome.modelassistant.service.in \
    > %{buildroot}%{_datadir}/dbus-1/services/org.gnome.modelassistant.service

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/org.gnome.modelassistant.desktop
appstreamcli validate --no-net --pedantic %{buildroot}%{_metainfodir}/org.gnome.modelassistant.metainfo.xml

%post
if [ -x %{_bindir}/dconf ]; then
    %{_bindir}/dconf update || :
fi

%postun
if [ -x %{_bindir}/dconf ]; then
    %{_bindir}/dconf update || :
fi

%files
%license LICENSE
%doc README.md examples/assistant.toml
%{_bindir}/model-assistant
%{_libexecdir}/%{name}/runner-keeper
%{_datadir}/applications/org.gnome.modelassistant.desktop
%{_metainfodir}/org.gnome.modelassistant.metainfo.xml
%{_datadir}/dbus-1/services/org.gnome.modelassistant.service
%{_datadir}/icons/hicolor/scalable/apps/org.gnome.modelassistant.svg
%config(noreplace) %{_sysconfdir}/dconf/db/distro.d/00-model-assistant-shortcuts
%config(noreplace) %{_sysconfdir}/dconf/db/distro.d/locks/00-model-assistant-shortcuts

%changelog
* Mon May 18 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.2.0-1
- Refactor the code

* Sat May 16 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.1.0-1
- Initial Fedora package
