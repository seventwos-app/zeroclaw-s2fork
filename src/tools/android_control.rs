use crate::config::AndroidConfig;
use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct AndroidControlTool {
    security: Arc<SecurityPolicy>,
    config: AndroidConfig,
}

impl AndroidControlTool {
    pub fn new(security: Arc<SecurityPolicy>, config: AndroidConfig) -> Self {
        Self { security, config }
    }

    /// Connect to the ADB device over TCP.
    fn connect_device(
        &self,
    ) -> anyhow::Result<adb_client::tcp::ADBTcpDevice> {
        use std::net::{IpAddr, SocketAddr};
        let ip: IpAddr = self
            .config
            .host
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid Android host IP '{}': {}", self.config.host, e))?;
        let addr = SocketAddr::new(ip, self.config.port);
        adb_client::tcp::ADBTcpDevice::new(addr)
            .map_err(|e| anyhow::anyhow!("ADB connection to {} failed: {}", addr, e))
    }

    /// Run a shell command on the device and capture stdout.
    fn shell_cmd(device: &mut adb_client::tcp::ADBTcpDevice, cmd: &str) -> anyhow::Result<Vec<u8>> {
        use adb_client::ADBDeviceExt;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        device.shell_command(&cmd, Some(&mut stdout), Some(&mut stderr))?;
        if !stderr.is_empty() {
            let err_str = String::from_utf8_lossy(&stderr);
            // Some commands write warnings to stderr but still succeed
            if stdout.is_empty() {
                anyhow::bail!("Command failed: {}", err_str.trim());
            }
        }
        Ok(stdout)
    }

    /// Parse uiautomator XML dump into a compact, token-efficient accessibility tree.
    fn parse_ui_state(xml: &str) -> String {
        let doc = match roxmltree::Document::parse(xml) {
            Ok(d) => d,
            Err(e) => return format!("Failed to parse UI XML: {}", e),
        };

        let mut nodes = Vec::new();
        let mut idx = 0usize;

        for node in doc.descendants() {
            if !node.is_element() {
                continue;
            }

            let clickable = node.attribute("clickable").unwrap_or("false") == "true";
            let scrollable = node.attribute("scrollable").unwrap_or("false") == "true";
            let text = node.attribute("text").unwrap_or("");
            let content_desc = node.attribute("content-desc").unwrap_or("");
            let resource_id = node.attribute("resource-id").unwrap_or("");
            let enabled = node.attribute("enabled").unwrap_or("true") == "true";
            let bounds = node.attribute("bounds").unwrap_or("");

            // Skip nodes that aren't interactive and have no useful text
            if !clickable && !scrollable && text.is_empty() && content_desc.is_empty() && resource_id.is_empty() {
                continue;
            }

            // Skip disabled nodes
            if !enabled {
                continue;
            }

            // Skip zero-size nodes (bounds like [0,0][0,0])
            if bounds == "[0,0][0,0]" {
                continue;
            }

            // Build compact representation
            let class_name = node.attribute("class").unwrap_or("View");
            // Strip android.widget. and android.view. prefixes
            let short_class = class_name
                .strip_prefix("android.widget.")
                .or_else(|| class_name.strip_prefix("android.view."))
                .unwrap_or(class_name);

            let mut parts = Vec::new();
            parts.push(format!("[{}] {}", idx, short_class));

            if !text.is_empty() {
                parts.push(format!("\"{}\"", text));
            }
            if !content_desc.is_empty() && content_desc != text {
                parts.push(format!("desc=\"{}\"", content_desc));
            }
            if !resource_id.is_empty() {
                // Strip package prefix from resource-id
                let short_id = resource_id
                    .rsplit_once('/')
                    .map(|(_, id)| id)
                    .unwrap_or(resource_id);
                parts.push(format!("id={}", short_id));
            }
            if clickable {
                parts.push("clickable".to_string());
            }
            if scrollable {
                parts.push("scrollable".to_string());
            }
            if !bounds.is_empty() {
                parts.push(format!("bounds={}", bounds));
            }

            nodes.push(parts.join(" "));
            idx += 1;
        }

        if nodes.is_empty() {
            "No interactive UI elements found".to_string()
        } else {
            nodes.join("\n")
        }
    }

    /// Map friendly key names to Android KEYCODE values.
    fn key_name_to_code(name: &str) -> Option<u32> {
        match name.to_lowercase().as_str() {
            "home" => Some(3),
            "back" => Some(4),
            "call" => Some(5),
            "endcall" | "end_call" => Some(6),
            "dpad_up" | "up" => Some(19),
            "dpad_down" | "down" => Some(20),
            "dpad_left" | "left" => Some(21),
            "dpad_right" | "right" => Some(22),
            "dpad_center" | "center" => Some(23),
            "volume_up" => Some(24),
            "volume_down" => Some(25),
            "power" => Some(26),
            "camera" => Some(27),
            "tab" => Some(61),
            "space" => Some(62),
            "enter" | "return" => Some(66),
            "delete" | "backspace" | "del" => Some(67),
            "menu" => Some(82),
            "search" => Some(84),
            "media_play_pause" | "play_pause" => Some(85),
            "media_stop" | "stop" => Some(86),
            "media_next" | "next" => Some(87),
            "media_previous" | "previous" => Some(88),
            "mute" => Some(91),
            "page_up" => Some(92),
            "page_down" => Some(93),
            "escape" | "esc" => Some(111),
            "forward_del" => Some(112),
            "recent" | "app_switch" => Some(187),
            "brightness_down" => Some(220),
            "brightness_up" => Some(221),
            "sleep" => Some(223),
            "wakeup" | "wake_up" => Some(224),
            // Allow raw keycode numbers
            _ => name.parse::<u32>().ok(),
        }
    }

    /// Escape text for `input text` command (spaces → %s, special chars).
    fn escape_input_text(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len() * 2);
        for ch in text.chars() {
            match ch {
                ' ' => escaped.push_str("%s"),
                '&' | '<' | '>' | '|' | ';' | '(' | ')' | '$' | '`' | '\\' | '"' | '\'' | '{' | '}' | '!' | '~' | '#' => {
                    escaped.push('\\');
                    escaped.push(ch);
                }
                _ => escaped.push(ch),
            }
        }
        escaped
    }
}

#[async_trait]
impl Tool for AndroidControlTool {
    fn name(&self) -> &str {
        "android_control"
    }

    fn description(&self) -> &str {
        "Control an Android device. Actions: wake (wake+unlock screen), screenshot, get_state, tap, long_tap, swipe, type_text, key, launch, shell, status."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["wake", "screenshot", "get_state", "tap", "long_tap", "swipe", "type_text", "key", "launch", "shell", "status"],
                    "description": "The action to perform on the Android device"
                },
                "x": {
                    "type": "integer",
                    "description": "X coordinate for tap/swipe"
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate for tap/swipe"
                },
                "x2": {
                    "type": "integer",
                    "description": "End X coordinate for swipe"
                },
                "y2": {
                    "type": "integer",
                    "description": "End Y coordinate for swipe"
                },
                "duration_ms": {
                    "type": "integer",
                    "description": "Duration in ms for swipe/long_tap"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type or key name (home, back, enter, menu, volume_up, volume_down, power, tab, delete, recent)"
                },
                "package": {
                    "type": "string",
                    "description": "Package name for launch (e.g. com.zhiliaoapp.musically)"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command for the shell action"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?
            .to_string();

        // Security: most actions are side-effecting
        let is_read_only = matches!(action.as_str(), "screenshot" | "get_state" | "status");
        if !is_read_only {
            if !self.security.can_act() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Action blocked: autonomy is read-only".into()),
                });
            }
            if !self.security.record_action() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Action blocked: rate limit exceeded".into()),
                });
            }
        }

        let config = self.config.clone();
        let args_clone = args.clone();
        let tool = self.clone_for_blocking();

        // All ADB calls are blocking — run in spawn_blocking
        let result = tokio::task::spawn_blocking(move || {
            tool.execute_action(&action, &args_clone, &config)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Android control task panicked: {}", e))?;

        match result {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Android control error: {}", e)),
            }),
        }
    }
}

impl AndroidControlTool {
    /// Clone the minimal state needed for the blocking task.
    fn clone_for_blocking(&self) -> BlockingAndroidControl {
        BlockingAndroidControl {
            config: self.config.clone(),
        }
    }
}

/// Minimal struct for executing ADB commands in a blocking context.
struct BlockingAndroidControl {
    config: AndroidConfig,
}

impl BlockingAndroidControl {
    fn connect(&self) -> anyhow::Result<adb_client::tcp::ADBTcpDevice> {
        use std::net::{IpAddr, SocketAddr};
        let ip: IpAddr = self
            .config
            .host
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid Android host IP '{}': {}", self.config.host, e))?;
        let addr = SocketAddr::new(ip, self.config.port);
        adb_client::tcp::ADBTcpDevice::new(addr)
            .map_err(|e| anyhow::anyhow!("ADB connection to {} failed: {}", addr, e))
    }

    fn shell_cmd(device: &mut adb_client::tcp::ADBTcpDevice, cmd: &str) -> anyhow::Result<String> {
        use adb_client::ADBDeviceExt;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        device.shell_command(&cmd, Some(&mut stdout), Some(&mut stderr))?;
        if !stderr.is_empty() && stdout.is_empty() {
            let err_str = String::from_utf8_lossy(&stderr);
            anyhow::bail!("{}", err_str.trim());
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    fn shell_cmd_raw(device: &mut adb_client::tcp::ADBTcpDevice, cmd: &str) -> anyhow::Result<Vec<u8>> {
        use adb_client::ADBDeviceExt;
        let mut stdout = Vec::new();
        device.shell_command(&cmd, Some(&mut stdout), None)?;
        Ok(stdout)
    }

    fn execute_action(
        &self,
        action: &str,
        args: &serde_json::Value,
        _config: &AndroidConfig,
    ) -> anyhow::Result<String> {
        match action {
            "wake" => self.action_wake(),
            "screenshot" => self.action_screenshot(),
            "get_state" => self.action_get_state(),
            "tap" => self.action_tap(args),
            "long_tap" => self.action_long_tap(args),
            "swipe" => self.action_swipe(args),
            "type_text" => self.action_type_text(args),
            "key" => self.action_key(args),
            "launch" => self.action_launch(args),
            "shell" => self.action_shell(args),
            "status" => self.action_status(),
            _ => anyhow::bail!("Unknown action: {}", action),
        }
    }

    fn action_wake(&self) -> anyhow::Result<String> {
        // Check if screen is already on
        let mut device = self.connect()?;
        let power_state = Self::shell_cmd(&mut device, "dumpsys power | grep 'Display Power'")?;
        let screen_on = power_state.contains("state=ON");

        if !screen_on {
            // Press power button to wake
            let mut device = self.connect()?;
            Self::shell_cmd(&mut device, "input keyevent 26")?; // KEYCODE_POWER
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        // Swipe up to dismiss lock screen (works with swipe/no-lock)
        let mut device = self.connect()?;
        let size = Self::shell_cmd(&mut device, "wm size")?;
        // Parse "Physical size: 1080x2400" → get width and height
        let (w, h) = size
            .lines()
            .find(|l| l.contains('x'))
            .and_then(|l| {
                let dims = l.rsplit_once(' ')?.1;
                let (ws, hs) = dims.split_once('x')?;
                Some((ws.trim().parse::<i64>().ok()?, hs.trim().parse::<i64>().ok()?))
            })
            .unwrap_or((1080, 2400));

        let cx = w / 2;
        let mut device = self.connect()?;
        Self::shell_cmd(&mut device, &format!("input swipe {} {} {} {} 300", cx, h * 3 / 4, cx, h / 4))?;

        std::thread::sleep(std::time::Duration::from_millis(500));

        // Verify screen is now on and unlocked
        let mut device = self.connect()?;
        let state = Self::shell_cmd(&mut device, "dumpsys power | grep 'Display Power'")?;
        if state.contains("state=ON") {
            if screen_on {
                Ok("Screen was already on, swiped to dismiss lock screen".to_string())
            } else {
                Ok("Screen woken up and unlocked".to_string())
            }
        } else {
            Ok("Wake attempted but screen state uncertain — try again".to_string())
        }
    }

    fn action_screenshot(&self) -> anyhow::Result<String> {
        use base64::Engine;
        let mut device = self.connect()?;
        let png_data = Self::shell_cmd_raw(&mut device, "screencap -p")?;
        if png_data.len() < 8 {
            anyhow::bail!("Screenshot data too small ({} bytes), device may not support screencap", png_data.len());
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
        Ok(format!("data:image/png;base64,{}", b64))
    }

    fn action_get_state(&self) -> anyhow::Result<String> {
        let mut device = self.connect()?;
        let xml = Self::shell_cmd(&mut device, "uiautomator dump /dev/tty")?;
        // uiautomator dump outputs XML after "UI hierchary dumped to: /dev/tty"
        // The actual XML starts with <?xml or <hierarchy
        let xml_start = xml
            .find("<?xml")
            .or_else(|| xml.find("<hierarchy"))
            .unwrap_or(0);
        let xml_content = &xml[xml_start..];
        Ok(AndroidControlTool::parse_ui_state(xml_content))
    }

    fn action_tap(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let x = args.get("x").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("tap requires 'x' parameter"))?;
        let y = args.get("y").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("tap requires 'y' parameter"))?;
        let mut device = self.connect()?;
        Self::shell_cmd(&mut device, &format!("input tap {} {}", x, y))?;
        Ok(format!("Tapped at ({}, {})", x, y))
    }

    fn action_long_tap(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let x = args.get("x").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("long_tap requires 'x' parameter"))?;
        let y = args.get("y").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("long_tap requires 'y' parameter"))?;
        let duration = args.get("duration_ms").and_then(|v| v.as_i64()).unwrap_or(1500);
        let mut device = self.connect()?;
        Self::shell_cmd(&mut device, &format!("input swipe {} {} {} {} {}", x, y, x, y, duration))?;
        Ok(format!("Long tapped at ({}, {}) for {}ms", x, y, duration))
    }

    fn action_swipe(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let x = args.get("x").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("swipe requires 'x' parameter"))?;
        let y = args.get("y").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("swipe requires 'y' parameter"))?;
        let x2 = args.get("x2").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("swipe requires 'x2' parameter"))?;
        let y2 = args.get("y2").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("swipe requires 'y2' parameter"))?;
        let duration = args.get("duration_ms").and_then(|v| v.as_i64()).unwrap_or(300);
        let mut device = self.connect()?;
        Self::shell_cmd(&mut device, &format!("input swipe {} {} {} {} {}", x, y, x2, y2, duration))?;
        Ok(format!("Swiped from ({}, {}) to ({}, {}) over {}ms", x, y, x2, y2, duration))
    }

    fn action_type_text(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let text = args.get("text").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("type_text requires 'text' parameter"))?;
        let escaped = AndroidControlTool::escape_input_text(text);
        let mut device = self.connect()?;
        Self::shell_cmd(&mut device, &format!("input text '{}'", escaped))?;
        Ok(format!("Typed: {}", text))
    }

    fn action_key(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let key = args.get("text").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("key requires 'text' parameter with key name"))?;
        let keycode = AndroidControlTool::key_name_to_code(key)
            .ok_or_else(|| anyhow::anyhow!("Unknown key: '{}'. Use: home, back, enter, menu, volume_up, volume_down, power, tab, delete, recent, or a numeric keycode.", key))?;
        let mut device = self.connect()?;
        Self::shell_cmd(&mut device, &format!("input keyevent {}", keycode))?;
        Ok(format!("Key pressed: {} (KEYCODE_{})", key, keycode))
    }

    fn action_launch(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let package = args.get("package").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("launch requires 'package' parameter"))?;
        // Validate package name format (basic safety check)
        if !package.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '_') {
            anyhow::bail!("Invalid package name: {}", package);
        }
        let mut device = self.connect()?;
        let output = Self::shell_cmd(&mut device, &format!("monkey -p {} -c android.intent.category.LAUNCHER 1", package))?;
        if output.contains("No activities found") {
            anyhow::bail!("Package '{}' not found or has no launcher activity", package);
        }
        Ok(format!("Launched {}", package))
    }

    fn action_shell(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let command = args.get("command").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("shell requires 'command' parameter"))?;
        let mut device = self.connect()?;
        let output = Self::shell_cmd(&mut device, command)?;
        // Truncate very long output to avoid blowing up context
        if output.len() > 8000 {
            Ok(format!("{}... [truncated, {} bytes total]", &output[..8000], output.len()))
        } else {
            Ok(output)
        }
    }

    fn action_status(&self) -> anyhow::Result<String> {
        let mut device = self.connect()?;

        let model = Self::shell_cmd(&mut device, "getprop ro.product.model")
            .unwrap_or_else(|_| "unknown".to_string());
        // Need fresh connection for each command (ADB protocol limitation)
        let mut device = self.connect()?;
        let android_ver = Self::shell_cmd(&mut device, "getprop ro.build.version.release")
            .unwrap_or_else(|_| "unknown".to_string());
        let mut device = self.connect()?;
        let battery = Self::shell_cmd(&mut device, "dumpsys battery")
            .unwrap_or_else(|_| "unavailable".to_string());
        let mut device = self.connect()?;
        let screen_size = Self::shell_cmd(&mut device, "wm size")
            .unwrap_or_else(|_| "unknown".to_string());
        let mut device = self.connect()?;
        let screen_state = Self::shell_cmd(&mut device, "dumpsys power | grep 'Display Power'")
            .unwrap_or_else(|_| "unknown".to_string());

        // Parse battery level from dumpsys output
        let battery_level = battery
            .lines()
            .find(|l| l.trim().starts_with("level:"))
            .map(|l| l.trim().trim_start_matches("level:").trim())
            .unwrap_or("?");
        let battery_status = battery
            .lines()
            .find(|l| l.trim().starts_with("status:"))
            .map(|l| l.trim().trim_start_matches("status:").trim())
            .unwrap_or("?");

        Ok(format!(
            "Model: {}\nAndroid: {}\nBattery: {}% (status: {})\nScreen: {}\nDisplay: {}",
            model.trim(),
            android_ver.trim(),
            battery_level,
            battery_status,
            screen_size.trim(),
            screen_state.trim(),
        ))
    }
}
