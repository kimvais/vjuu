#!/usr/bin/env bash
# Builds a signed apt repository from the .deb files in $1 into $2.
# Needs the armoured private signing key in $KEY. Users add it with:
#   curl -fsSL https://kimvais.github.io/vjuu/apt/vjuu.gpg | sudo tee /usr/share/keyrings/vjuu.gpg >/dev/null
#   echo "deb [signed-by=/usr/share/keyrings/vjuu.gpg] https://kimvais.github.io/vjuu/apt stable main" \
#     | sudo tee /etc/apt/sources.list.d/vjuu.list
#   sudo apt update && sudo apt install vjuu
set -euo pipefail
debs=$1
out=$2

GNUPGHOME=$(mktemp -d)
export GNUPGHOME
gpg --batch --quiet --import <<<"$KEY"
fingerprint=$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')

mkdir -p "$out/pool/main/v/vjuu"
cp "$debs"/*.deb "$out/pool/main/v/vjuu/"
cd "$out"
for arch in amd64 arm64; do
    mkdir -p "dists/stable/main/binary-$arch"
    apt-ftparchive --arch "$arch" packages pool > "dists/stable/main/binary-$arch/Packages"
    gzip -9 -k -f "dists/stable/main/binary-$arch/Packages"
done
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=vjuu \
    -o APT::FTPArchive::Release::Label=vjuu \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
    -o APT::FTPArchive::Release::Components=main \
    release dists/stable > dists/stable/Release
gpg --batch --yes --local-user "$fingerprint" --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --batch --yes --local-user "$fingerprint" --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
gpg --batch --export "$fingerprint" > vjuu.gpg
