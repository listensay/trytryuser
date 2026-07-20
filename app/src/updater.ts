import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import type { MessageInstance } from "antd/es/message/interface";
import type { HookAPI as ModalHookAPI } from "antd/es/modal/useModal";

/**
 * 检查并安装更新。
 *
 * @param message  antd 的 message 实例（轻提示）
 * @param modal    antd 的 modal 实例（确认弹窗）
 * @param silent   true 时若无更新则静默（用于启动自动检查），false 时会提示"已是最新"
 */
export async function checkForUpdates(
  message: MessageInstance,
  modal: ModalHookAPI,
  silent = false,
): Promise<void> {
  let update: Update | null = null;
  try {
    update = await check();
  } catch (error) {
    if (!silent) {
      message.error(`检查更新失败：${String(error)}`);
    }
    return;
  }

  if (!update) {
    if (!silent) {
      message.success("当前已是最新版本");
    }
    return;
  }

  modal.confirm({
    title: `发现新版本 ${update.version}`,
    content: update.body?.trim()
      ? update.body
      : "已有可用更新，是否立即下载并安装？安装完成后应用会自动重启。",
    okText: "立即更新",
    cancelText: "以后再说",
    width: 460,
    onOk: async () => {
      const hide = message.loading("正在下载更新…", 0);
      try {
        let downloaded = 0;
        let total = 0;
        await update!.downloadAndInstall((event) => {
          switch (event.event) {
            case "Started":
              total = event.data.contentLength ?? 0;
              break;
            case "Progress":
              downloaded += event.data.chunkLength;
              if (total > 0) {
                const percent = Math.floor((downloaded / total) * 100);
                hide();
                message.loading(`正在下载更新… ${percent}%`, 0);
              }
              break;
            case "Finished":
              break;
          }
        });
        hide();
        message.success("下载完成，即将重启应用");
        await relaunch();
      } catch (error) {
        hide();
        message.error(`更新失败：${String(error)}`);
      }
    },
  });
}
