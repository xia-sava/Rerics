# PDFium 同梱バイナリ

PDF ページのラスタライズに用いる PDFium の事前ビルド動的ライブラリ。

- 取得元: https://github.com/bblanchon/pdfium-binaries
- リリース: chromium/7763（PDFium 148.0.7763.0）＝`pdfium-render 0.9.2` の既定バインディング（`pdfium_latest`）が想定する版
- 対象: Windows x64
- `pdfium.dll` はビルド時に `build.rs` が実行ファイルと同じディレクトリへコピーし、
  実行時に `Pdfium::bind_to_library` で動的ロードする。

## ライセンス

- `LICENSE` … 配布物のライセンス（MIT・Benoit Blanchon）
- `licenses/pdfium.txt` … PDFium 本体（BSD-3-Clause・The PDFium Authors）
- `licenses/` … 同梱される第三者ライブラリの各ライセンス

## 再取得

```sh
curl -L -o pdfium.tgz "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7763/pdfium-win-x64.tgz"
tar xzf pdfium.tgz
cp bin/pdfium.dll vendor/pdfium/pdfium.dll
cp LICENSE vendor/pdfium/LICENSE
cp -r licenses vendor/pdfium/licenses
```
