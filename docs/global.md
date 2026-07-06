实现一个Rust即时回放软件，编码与录制循环参见"C:\Users\Administrator\Desktop\RustReplay\docs\encoder.md"，GUI标准参见"C:\Users\Administrator\Desktop\RustReplay\docs\GUI.md"
1，使用简体中文写说明和注释
2，尽量使用子代理，但需要注意文件并发写问题
3，在本地写代码，只向远端传编译产物并调试
4，发布包允许打包依赖；程序主体是 Rust exe，libvpl.dll 等用户态依赖可随包携带，GPU 驱动/D3D11/Media Foundation 属于系统前提
5，请使用相对目录
6，调试机有免密ssh administrator@10.230.1.103，旧调试阶段优先使用接口文档.md中的/run接口；成品 exe 启动即打开 GUI，不保留调试 CLI
7，文件传输若遇到困难可使用Y:\transfer，这是一个SMB卷，在本地（Buckle）映射为Y:\transfer，在远端（Anywhere）映射为Z:\transfer
