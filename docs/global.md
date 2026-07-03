实现一个单文件Rust即时回放软件，编码与录制循环参见"C:\Users\Administrator\Desktop\RustReplay\docs\encoder.md"，GUI标准参见"C:\Users\Administrator\Desktop\RustReplay\docs\GUI.md"
1，使用简体中文写说明和注释
2，尽量使用子代理，但需要注意文件并发写问题
3，在本地写代码，只向远端传编译产物并调试
4，目标为exe单文件
5，请使用相对目录
6，调试机有免密ssh administrator@10.230.1.103，但依然请优先使用接口文档.md中的/run接口进行调试
7，文件传输若遇到困难可使用Y:\transfer，这是一个SMB卷，在本地（Buckle）映射为Y:\transfer，在远端（Anywhere）映射为Z:\transfer
