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
