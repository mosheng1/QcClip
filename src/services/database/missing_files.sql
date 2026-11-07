-- 失效图片/文件的隐藏关系。可重建缓存：扫描按内容 ID 整体替换，
-- 内容被删除时随外键级联清理；导入数据后清空并重新扫描。
CREATE TABLE IF NOT EXISTS hidden_missing_items (
    item_id INTEGER PRIMARY KEY
        REFERENCES clipboard_items(id) ON DELETE CASCADE
);
