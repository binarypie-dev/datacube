%global crate datacube

Name:           %{crate}
Version:        0.1.4
Release:        1%{?dist}
Summary:        Data provider service for application launchers and desktop utilities

License:        Apache-2.0
URL:            https://github.com/binarypie-dev/datacube
Source0:        %{url}/archive/main/%{crate}-main.tar.gz

BuildRequires:  rust >= 1.70
BuildRequires:  cargo
BuildRequires:  protobuf-compiler
BuildRequires:  systemd-rpm-macros

%description
Datacube is a background service that provides data to application launchers
and desktop utilities. It indexes desktop applications, provides calculator
functionality, and command execution capabilities via a Unix socket interface.

%prep
%autosetup -n %{crate}-main

%build
cargo build --release --locked

%install
# Install binaries
install -Dm755 target/release/datacube %{buildroot}%{_bindir}/datacube
install -Dm755 target/release/datacube-cli %{buildroot}%{_bindir}/datacube-cli

# Install systemd user service
install -Dm644 datacube.service %{buildroot}%{_userunitdir}/datacube.service

%post
%systemd_user_post %{crate}.service

%preun
%systemd_user_preun %{crate}.service

%postun
%systemd_user_postun_with_restart %{crate}.service

%files
%license LICENSE
%doc README.md
%{_bindir}/datacube
%{_bindir}/datacube-cli
%{_userunitdir}/datacube.service
