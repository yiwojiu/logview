//! 资源管理器右键菜单集成（仅 Windows 真正做事）。
//!
//! 目标有两个，缺一个都不够用：
//!
//! 1. 任意文件的右键菜单里出现「用 logview 打开」——用户想拿它看 `.txt`、`.out`、
//!    `.jsonl` 时不该先去改扩展名，所以挂在 `*` 上，不是挂在 `.log` 上。
//! 2. 任意文件的「打开方式 → 选择其他应用」里也有 logview——只有第 1 条的话，
//!    双击文件仍然打不开，"以后都用这个打开"这一步也就无从设置。
//!
//! 三处注册表项全部落在 **HKCU**：不碰 HKLM、不需要管理员权限，
//! "移除"就是把它们删掉，系统里不留别的东西。
//!
//! 为什么不调 `reg.exe` 而是自己声明 advapi32 的几个函数：最初那版就是起
//! `reg.exe`（省事、无 unsafe），但实测在企业安全策略下它会**整个进程被拦**——
//! 进程起不来，连退出码都拿不到，用户只看到一句"调不起 reg.exe"，功能直接废掉。
//! 注册表读写本来就是进程内的系统调用，不该被这种黑名单牵连。多出来的成本
//! 是几十行 `unsafe` 与宽字符缓冲，一次写清就够。
//!
//! 关于 Windows 11：挂 `*` 的经典菜单项属于"完整菜单"，默认收起在
//! 「显示更多选项」（Shift+F10）里。要让它出现在一级菜单，得提供
//! `IExplorerCommand` 的 COM 组件并打成 MSIX 包——那已经不是一个日志查看器
//! 该背的发布流程了。界面上如实写明这一点，别让用户以为没装成功。

/// 右键菜单项。挂在 `*` 上，所有文件都有。
const VERB_KEY: &str = r"Software\Classes\*\shell\Logview";
const COMMAND_KEY: &str = r"Software\Classes\*\shell\Logview\command";
/// 「打开方式」列表里的程序条目。键名用 exe 文件名是 Windows 的约定。
const APP_KEY: &str = r"Software\Classes\Applications\logview.exe";
const APP_COMMAND_KEY: &str = r"Software\Classes\Applications\logview.exe\shell\open\command";

/// 菜单文案与「打开方式」里显示的程序名
const MENU_LABEL: &str = "用 logview 打开";
const APP_LABEL: &str = "logview 日志查看器";

/// 旧版按扩展名注册的那几项（v0.1.17 之前手工配置留下的）。
///
/// 留着会和新的 `*` 项在 `.log` 上叠成两条一模一样的菜单项，
/// 所以在安装与卸载时都顺手清掉。
const LEGACY_VERB_KEY: &str = r"Software\Classes\SystemFileAssociations\.log\shell\OpenWithLogview";
const LEGACY_PROGID_KEY: &str = r"Software\Classes\Logview.Log";
const LEGACY_OPENWITH_KEY: &str = r"Software\Classes\.log\OpenWithProgids";
const LEGACY_OPENWITH_VALUE: &str = "Logview.Log";

/// 当前平台能不能做这件事。
pub const SUPPORTED: bool = cfg!(windows);

/// 集成状态。`installed` 只看注册表在不在，路径对不对是另一件事。
pub struct Status {
    pub installed: bool,
    /// 注册表里记下的 exe 路径（从 command 的默认值里抠出来的）
    pub registered_exe: Option<String>,
    /// 本次运行的程序路径
    pub current_exe: String,
}

impl Status {
    /// 注册过了，但指的已经不是当前这个 exe（换了目录、或者从 Release 包换成自编译）。
    ///
    /// 这种情况菜单项仍然在，点下去打开的是旧路径——要么报错，要么打开旧版本，
    /// 都得在界面上说清楚，而不是只显示"已启用"。
    pub fn points_elsewhere(&self) -> bool {
        match &self.registered_exe {
            Some(p) => !self.current_exe.is_empty() && !same_path(p, &self.current_exe),
            None => false,
        }
    }
}

/// 路径比较忽略大小写：Windows 上同一个文件的大小写写法不唯一。
fn same_path(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

pub fn current_exe() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// 菜单项实际执行的那条命令。
///
/// `%1` 是被右键的那个文件，资源管理器会把它换成完整路径；
/// 加引号是因为路径里带空格太常见。
fn open_command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// 从命令行里抠出 exe 路径。
///
/// 存进去的是 `"C:\...\logview.exe" "%1"`，所以第一对引号之间就是程序路径。
/// 不用 `split` 之类：路径里带空格是常态，靠分隔符切不靠谱。
fn exe_from_command(cmd: &str) -> Option<String> {
    let start = cmd.find('"')? + 1;
    let rest = &cmd[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub fn status() -> Result<Status, String> {
    let current_exe = current_exe();

    #[cfg(not(windows))]
    {
        Ok(Status {
            installed: false,
            registered_exe: None,
            current_exe,
        })
    }

    #[cfg(windows)]
    {
        let cmd = win::get_string(COMMAND_KEY, None)?;
        Ok(Status {
            installed: cmd.is_some(),
            registered_exe: cmd.as_deref().and_then(exe_from_command),
            current_exe,
        })
    }
}

pub fn install() -> Result<(), String> {
    #[cfg(not(windows))]
    {
        Err("右键菜单集成目前只支持 Windows".to_string())
    }

    #[cfg(windows)]
    {
        let exe = current_exe();
        if exe.is_empty() {
            return Err("拿不到当前程序路径".to_string());
        }
        let cmd = open_command(&exe);
        remove_legacy();

        win::set_string(VERB_KEY, None, MENU_LABEL)?;
        win::set_string(VERB_KEY, Some("Icon"), &format!("{exe},0"))?;
        win::set_string(COMMAND_KEY, None, &cmd)?;

        // 「打开方式」列表：默认值当显示名，再补一个友好名称
        win::set_string(APP_KEY, None, APP_LABEL)?;
        win::set_string(APP_KEY, Some("FriendlyAppName"), APP_LABEL)?;
        win::set_string(APP_COMMAND_KEY, None, &cmd)?;
        Ok(())
    }
}

pub fn uninstall() -> Result<(), String> {
    #[cfg(not(windows))]
    {
        Err("右键菜单集成目前只支持 Windows".to_string())
    }

    #[cfg(windows)]
    {
        win::delete_tree(VERB_KEY)?;
        win::delete_tree(APP_KEY)?;
        remove_legacy();
        Ok(())
    }
}

/// 清掉旧版按扩展名注册的项，避免 `.log` 上出现两条同名菜单项。
fn remove_legacy() {
    #[cfg(windows)]
    {
        let _ = win::delete_tree(LEGACY_VERB_KEY);
        let _ = win::delete_tree(LEGACY_PROGID_KEY);
        let _ = win::delete_value(LEGACY_OPENWITH_KEY, LEGACY_OPENWITH_VALUE);
    }
}

/// advapi32 的最小封装：只够本项目用，别当通用注册表库看。
#[cfg(windows)]
mod win {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    /// 预定义句柄 `HKEY_CURRENT_USER` 是 `(HKEY)0x80000001`。
    /// 64 位下要先经 `i32` 符号扩展成 `0xFFFFFFFF80000001`：直接 `as isize`
    /// 会得到正数 0x0000000080000001，advapi32 认不出来。
    const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;

    const KEY_READ: u32 = 0x2_0019;
    const KEY_WRITE: u32 = 0x2_0006;
    const REG_SZ: u32 = 1;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_FILE_NOT_FOUND: i32 = 2;

    #[link(name = "advapi32")]
    extern "system" {
        #[link_name = "RegCreateKeyExW"]
        fn create_key_ex(
            hkey: isize,
            sub_key: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            sam: u32,
            security: *const core::ffi::c_void,
            result: *mut isize,
            disposition: *mut u32,
        ) -> i32;

        #[link_name = "RegOpenKeyExW"]
        fn open_key_ex(
            hkey: isize,
            sub_key: *const u16,
            options: u32,
            sam: u32,
            result: *mut isize,
        ) -> i32;

        #[link_name = "RegSetValueExW"]
        fn set_value_ex(
            hkey: isize,
            name: *const u16,
            reserved: u32,
            ty: u32,
            data: *const u8,
            len: u32,
        ) -> i32;

        #[link_name = "RegQueryValueExW"]
        fn query_value_ex(
            hkey: isize,
            name: *const u16,
            reserved: *mut u32,
            ty: *mut u32,
            data: *mut u8,
            len: *mut u32,
        ) -> i32;

        #[link_name = "RegCloseKey"]
        fn close_key(hkey: isize) -> i32;

        #[link_name = "RegDeleteTreeW"]
        fn delete_tree_raw(hkey: isize, sub_key: *const u16) -> i32;

        #[link_name = "RegDeleteKeyValueW"]
        fn delete_key_value_raw(hkey: isize, sub_key: *const u16, name: *const u16) -> i32;
    }

    /// 宽字符 + 结尾 0。这几个 API 都只认这一种形态。
    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn name_ptr(name: Option<&str>) -> (Option<Vec<u16>>, *const u16) {
        let w = name.map(wide);
        let ptr = w.as_ref().map_or(ptr::null(), |v| v.as_ptr());
        (w, ptr)
    }

    /// 系统自带的错误说明是本地化的，比我们自己编一句"失败"有用。
    fn err(code: i32, what: &str) -> String {
        format!("{what}：{}", std::io::Error::from_raw_os_error(code))
    }

    /// 打开或新建一个子键。
    fn create(path: &str) -> Result<isize, String> {
        let sub = wide(path);
        let mut key: isize = 0;
        let mut disposition: u32 = 0;
        // SAFETY: 指针都指向本函数栈上的有效变量；sub 以 0 结尾；
        // security 传 null 是文档允许的（沿用父键的 ACL）。
        let code = unsafe {
            create_key_ex(
                HKEY_CURRENT_USER,
                sub.as_ptr(),
                0,
                ptr::null(),
                0,
                KEY_WRITE,
                ptr::null(),
                &mut key,
                &mut disposition,
            )
        };
        if code != ERROR_SUCCESS {
            return Err(err(code, "打开注册表项失败"));
        }
        Ok(key)
    }

    /// 写一个字符串值。`name` 为 None 表示写默认值。
    pub fn set_string(path: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let key = create(path)?;
        let (_keep, name_ptr) = name_ptr(name);
        let data = wide(value);
        // SAFETY: key 由 create() 打开且在本函数内关闭；name_ptr 活着（_keep 兜住它的生命周期）；
        // data 的字节数按 u16 个数换算，含结尾 0，正是 REG_SZ 的要求。
        let code = unsafe {
            set_value_ex(
                key,
                name_ptr,
                0,
                REG_SZ,
                data.as_ptr().cast::<u8>(),
                (data.len() * 2) as u32,
            )
        };
        // SAFETY: key 是刚打开的有效句柄，只关闭这一次。
        unsafe { close_key(key) };
        if code != ERROR_SUCCESS {
            return Err(err(code, "写入注册表失败"));
        }
        Ok(())
    }

    /// 读一个字符串值。键或值不存在时返回 `Ok(None)`——这不是错误。
    pub fn get_string(path: &str, name: Option<&str>) -> Result<Option<String>, String> {
        let sub = wide(path);
        let mut key: isize = 0;
        // SAFETY: sub 以 0 结尾，key 指向本栈变量。
        let code = unsafe { open_key_ex(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut key) };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if code != ERROR_SUCCESS {
            return Err(err(code, "读取注册表失败"));
        }

        let result = read_value(key, name);
        // SAFETY: key 由 open_key_ex 打开，只关闭一次。
        unsafe { close_key(key) };
        result
    }

    fn read_value(key: isize, name: Option<&str>) -> Result<Option<String>, String> {
        let (_keep, name_ptr) = name_ptr(name);

        // 先问长度：值可能比任何固定缓冲区都长
        let mut len: u32 = 0;
        // SAFETY: 数据指针传 null 是 Win32 查询所需缓冲区大小的标准用法。
        let code = unsafe {
            query_value_ex(
                key,
                name_ptr,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut len,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if code != ERROR_SUCCESS {
            return Err(err(code, "读取注册表值失败"));
        }

        // 多留一个 u16：注册表里的字符串不保证以 0 结尾
        let mut buf = vec![0u16; (len as usize / 2) + 1];
        let mut got: u32 = len;
        // SAFETY: buf 按查询到的字节数分配，长度指针与实际容量一致。
        let code = unsafe {
            query_value_ex(
                key,
                name_ptr,
                ptr::null_mut(),
                ptr::null_mut(),
                buf.as_mut_ptr().cast::<u8>(),
                &mut got,
            )
        };
        if code != ERROR_SUCCESS {
            return Err(err(code, "读取注册表值失败"));
        }
        let chars = (got as usize / 2).min(buf.len());
        let text: Vec<u16> = buf[..chars]
            .iter()
            .copied()
            .take_while(|c| *c != 0)
            .collect();
        Ok(Some(String::from_utf16_lossy(&text)))
    }

    /// 删掉整棵子键。本来就不存在算成功——"移除"是个幂等动作。
    pub fn delete_tree(path: &str) -> Result<(), String> {
        let sub = wide(path);
        // SAFETY: sub 以 0 结尾；这个 API 不返回句柄，无需释放。
        let code = unsafe { delete_tree_raw(HKEY_CURRENT_USER, sub.as_ptr()) };
        if code == ERROR_SUCCESS || code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(err(code, "删除注册表项失败"))
    }

    /// 删掉某个键下的单个值（键本身留着）。
    pub fn delete_value(path: &str, name: &str) -> Result<(), String> {
        let sub = wide(path);
        let name_w = wide(name);
        // SAFETY: 两个入参都以 0 结尾且活到调用结束。
        let code =
            unsafe { delete_key_value_raw(HKEY_CURRENT_USER, sub.as_ptr(), name_w.as_ptr()) };
        if code == ERROR_SUCCESS || code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(err(code, "删除注册表值失败"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_both_paths() {
        assert_eq!(
            open_command(r"C:\Program Files\logview.exe"),
            r#""C:\Program Files\logview.exe" "%1""#
        );
    }

    #[test]
    fn exe_is_read_between_the_first_quote_pair() {
        assert_eq!(
            exe_from_command(r#""C:\tools\logview.exe" "%1""#).as_deref(),
            Some(r"C:\tools\logview.exe")
        );
    }

    #[test]
    fn exe_is_none_when_there_are_no_quotes() {
        assert!(exe_from_command("logview.exe %1").is_none());
    }

    #[test]
    fn status_notices_a_moved_binary() {
        let moved = Status {
            installed: true,
            registered_exe: Some(r"C:\old\logview.exe".to_string()),
            current_exe: r"C:\new\logview.exe".to_string(),
        };
        assert!(moved.points_elsewhere());

        let same = Status {
            installed: true,
            registered_exe: Some(r"C:\NEW\Logview.exe".to_string()),
            current_exe: r"C:\new\logview.exe".to_string(),
        };
        assert!(!same.points_elsewhere());
    }
}
