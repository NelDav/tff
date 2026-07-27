# Maintainer: David Nelles <david.nelles@gmx.de>
pkgname=tff-git
pkgver=r1.0000000
pkgrel=1
pkgdesc="A node-based TUI for ffmpeg"
arch=('x86_64' 'aarch64')
url="https://github.com/NelDav/tff"
license=('MIT')
depends=('ffmpeg')
optdepends=('mpv: terminal video preview when no display is available (e.g. over SSH)')
makedepends=('cargo' 'git')
provides=('tff')
conflicts=('tff')
source=("$pkgname::git+https://github.com/NelDav/tff.git")
sha256sums=('SKIP')

pkgver() {
  cd "$pkgname"
  printf "r%s.%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd "$pkgname"
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$pkgname"
  cargo build --frozen --release
}

check() {
  cd "$pkgname"
  cargo test --frozen --release
}

package() {
  cd "$pkgname"
  install -Dm0755 -t "$pkgdir/usr/bin/" "target/release/tff"
  install -Dm0644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
