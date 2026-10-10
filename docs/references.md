# 参考資料の記録

機能ごとに、実装で参考にした資料を記録する（design.md 12.1）。
GPLのソースを参考にした場合は、仕組みを**コードを含まない自分の言葉**でここにまとめ、実装はこのメモと一次資料から行う。

## blocksource

| 内容 | 資料 |
|---|---|
| macOS の生デバイスの大きさ取得（`DKIOCGETBLOCKSIZE` / `DKIOCGETBLOCKCOUNT`） | macOS SDK `<sys/disk.h>` |
| Linux のブロックデバイスの大きさ取得（`BLKSSZGET` / `BLKGETSIZE64`） | Linux `<linux/fs.h>` |
| Windows のディスクの大きさ取得（`IOCTL_DISK_GET_LENGTH_INFO` / `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`） | Microsoft Learn（Win32 API ドキュメント） |
| 疑似乱数（SplitMix64） | Steele, Lea, Flood "Fast Splittable Pseudorandom Number Generators" (OOPSLA 2014) |

## tools/scenario（シナリオ生成器）

| 内容 | 資料 |
|---|---|
| exFAT のブートセクタ（クラスタサイズの読み取り） | Microsoft "exFAT file system specification" 3.1.14、3.1.15 |
| EXIF（APP1、TIFF IFD、Make / Model / DateTimeOriginal） | CIPA DC-008（Exif 2.3）、TIFF Revision 6.0 |
| JPEG のセグメント構造（SOI、APPn、COM） | ITU-T T.81 Annex B |
| MP4 の box 構造（ftyp / mdat / moov、stbl の各表） | ISO/IEC 14496-12（ISO BMFF） |
| AVC のサンプルエントリ（avc1 / avcC）、長さ付き NAL 単位 | ISO/IEC 14496-15 |
