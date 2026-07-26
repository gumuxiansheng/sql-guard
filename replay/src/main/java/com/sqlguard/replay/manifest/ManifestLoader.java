package com.sqlguard.replay.manifest;

import com.fasterxml.jackson.databind.ObjectMapper;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;

/**
 * 加载 sql-manifest.json。
 *
 * <p>对清单 {@code version} 字段做校验：当前实现支持版本 1。
 * 加载到不支持的版本时抛 {@link UnsupportedManifestVersionException}，
 * 避免未来清单格式变化时静默兼容失败导致字段缺失或解析错位。
 */
public final class ManifestLoader {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    /** 当前实现支持的清单版本。 */
    public static final int SUPPORTED_VERSION = 1;

    private ManifestLoader() {
    }

    /**
     * 从文件加载清单，并校验 version 字段。
     *
     * @throws UnsupportedManifestVersionException 当 version 缺失或不等于 {@link #SUPPORTED_VERSION}
     */
    public static Manifest load(Path manifestPath) throws IOException {
        Manifest m;
        try (java.io.InputStream in = Files.newInputStream(manifestPath)) {
            m = MAPPER.readValue(in, Manifest.class);
        }
        if (m.getVersion() != SUPPORTED_VERSION) {
            throw new UnsupportedManifestVersionException(
                    m.getVersion(), SUPPORTED_VERSION, manifestPath);
        }
        if (m.getStatements() == null) {
            m.setStatements(java.util.Collections.<ManifestStatement>emptyList());
        }
        return m;
    }
}
