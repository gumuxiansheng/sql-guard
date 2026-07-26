package com.sqlguard.replay.manifest;

import java.io.IOException;
import java.nio.file.Path;

/**
 * 清单 version 字段与当前实现不兼容时抛出。
 *
 * <p>继承 {@link IOException}，便于调用方在 {@code throws IOException} 签名下统一捕获。
 */
public final class UnsupportedManifestVersionException extends IOException {

    private static final long serialVersionUID = 1L;

    private final int actualVersion;
    private final int supportedVersion;

    public UnsupportedManifestVersionException(int actualVersion,
                                               int supportedVersion,
                                               Path manifestPath) {
        super("Unsupported manifest version: got " + actualVersion
                + ", expected " + supportedVersion
                + (manifestPath == null ? "" : " (file: " + manifestPath + ")")
                + ". This version of sqlguard-replay only supports manifest version "
                + supportedVersion + ". Regenerate the manifest with a matching "
                + "sqlguard build or upgrade sqlguard-replay.");
        this.actualVersion = actualVersion;
        this.supportedVersion = supportedVersion;
    }

    public int getActualVersion() {
        return actualVersion;
    }

    public int getSupportedVersion() {
        return supportedVersion;
    }
}
