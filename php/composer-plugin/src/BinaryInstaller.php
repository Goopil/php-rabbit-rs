<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Installer;

use Composer\Composer;
use Composer\IO\IOInterface;
use Composer\Package\PackageInterface;
use Composer\Util\HttpDownloader;
use ZipArchive;
use RuntimeException;

final class BinaryInstaller
{
    private const PACKAGE_NAME = 'rabbit-rs/installer';
    private const MANIFEST_FILE = 'manifest.json';
    private const DEFAULT_DOWNLOAD_TEMPLATE = 'https://github.com/Goopil/rabbit-rs/releases/download/%tag%/%file%';

    public function __construct(
        private readonly Composer $composer,
        private readonly IOInterface $io,
    ) {
    }

    public function install(): void
    {
        $platform = Platform::detect();
        $phpLabel = $this->resolvePhpLabel();
        $versionTag = $this->resolveVersionTag();

        $artifact = sprintf('rabbit_rs-%s-php%s.zip', $platform->id(), $phpLabel);
        $downloadUrl = $this->resolveDownloadUrl($versionTag, $artifact);

        $vendorDir = $this->composer->getConfig()->get('vendor-dir');
        $destination = sprintf('%s/goopil/rabbit-rs/ext/%s/php%s', $vendorDir, $platform->id(), $phpLabel);
        $manifestPath = $destination . '/' . self::MANIFEST_FILE;

        if (!$this->forceInstall() && $this->isAlreadyInstalled($manifestPath, $versionTag)) {
            $this->io->write(sprintf('<info>[rabbit-rs]</info> Reusing cached binary for %s (%s).', $platform->id(), $phpLabel));
            $this->publishStub($vendorDir);
            return;
        }

        $this->io->write(sprintf('<info>[rabbit-rs]</info> Downloading %s', $downloadUrl));
        $tmpZip = $this->download($downloadUrl);

        try {
            $this->unpack($tmpZip, $destination);
        } finally {
            @unlink($tmpZip);
        }
        $this->writeManifest($manifestPath, $versionTag, $downloadUrl);
        $this->publishStub($vendorDir);

        $this->io->write(sprintf('<info>[rabbit-rs]</info> Installed extension into %s', $destination));
    }

    public function publishStub(?string $vendorDir = null): void
    {
        $vendorDir ??= $this->composer->getConfig()->get('vendor-dir');
        $targetDir = $vendorDir . '/goopil/rabbit-rs/stubs';
        $source = __DIR__ . '/../stubs/RabbitRs.stub.php';

        if (!is_dir($targetDir) && !mkdir($targetDir, 0777, true) && !is_dir($targetDir)) {
            throw new RuntimeException(sprintf('Unable to create stub directory: %s', $targetDir));
        }

        if (!copy($source, $targetDir . '/RabbitRs.stub.php')) {
            throw new RuntimeException('Failed to copy RabbitRs stub.');
        }
    }

    private function forceInstall(): bool
    {
        return getenv('RABBIT_RS_FORCE_INSTALL') === '1';
    }

    private function resolvePhpLabel(): string
    {
        $major = PHP_MAJOR_VERSION;
        $minor = PHP_MINOR_VERSION;
        $version = PHP_VERSION;

        if (strips($version, 'RC') !== false || strips($version, 'beta') !== false) {
            return sprintf('%d.%d-rc', $major, $minor);
        }

        return sprintf('%d.%d', $major, $minor);
    }

    private function resolveVersionTag(): string
    {
        $env = getenv('RABBIT_RS_VERSION');
        if (is_string($env) && $env !== '') {
            return $this->normaliseTag($env);
        }

        $extra = $this->composer->getPackage()->getExtra();
        if (isset($extra['rabbit-rs']['version']) && is_string($extra['rabbit-rs']['version'])) {
            return $this->normaliseTag($extra['rabbit-rs']['version']);
        }

        $pluginPackage = $this->locatePluginPackage();
        if ($pluginPackage instanceof PackageInterface) {
            return $this->normaliseTag($pluginPackage->getPrettyVersion());
        }

        throw new RuntimeException(
            'Unable to resolve PhpRabbitRs release version. Set RABBIT_RS_VERSION or configure extra.rabbit-rs.version.'
        );
    }

    private function normaliseTag(string $raw): string
    {
        $raw = trim($raw);
        if ($raw === '') {
            throw new RuntimeException('Empty PhpRabbitRs version tag.');
        }

        // For semver tags without 'v' prefix, we don't add 'v'
        return $raw;
    }

    private function resolveDownloadUrl(string $tag, string $artifact): string
    {
        $extra = $this->composer->getPackage()->getExtra();
        $template = self::DEFAULT_DOWNLOAD_TEMPLATE;

        if (isset($extra['rabbit-rs']['download_template']) && is_string($extra['rabbit-rs']['download_template'])) {
            $template = $extra['rabbit-rs']['download_template'];
        }

        if (!str_contains($template, '%tag%') || !str_contains($template, '%file%')) {
            return sprintf($template, $tag, $artifact);
        }

        return str_replace(
            ['%tag%', '%file%'],
            [$tag, $artifact],
            $template
        );
    }

    private function download(string $url): string
    {
        $downloader = new HttpDownloader($this->io, $this->composer->getConfig());
        $response = $downloader->get($url);
        $body = $response->getBody();

        if ($body === null || $body === '') {
            throw new RuntimeException(sprintf('Empty response when downloading %s', $url));
        }

        $tmp = tempnam(sys_get_temp_dir(), 'rabbit-rs-');
        if ($tmp === false) {
            throw new RuntimeException('Unable to allocate temporary file for download.');
        }

        file_put_contents($tmp, $body);

        return $tmp;
    }

    private function unpack(string $zipPath, string $destination): void
    {
        if (!is_dir($destination) && !mkdir($destination, 0777, true) && !is_dir($destination)) {
            throw new RuntimeException(sprintf('Unable to create installation directory: %s', $destination));
        }

        $zip = new ZipArchive();
        if ($zip->open($zipPath) !== true) {
            throw new RuntimeException(sprintf('Failed to open archive: %s', $zipPath));
        }

        if (!$zip->extractTo($destination)) {
            throw new RuntimeException(sprintf('Failed to extract archive to: %s', $destination));
        }

        $zip->close();
    }

    private function writeManifest(string $manifestPath, string $version, string $downloadUrl): void
    {
        $payload = [
            'version' => $version,
            'download_url' => $downloadUrl,
            'installed_at' => date(DATE_ATOM),
            'php_version' => PHP_VERSION,
            'platform' => Platform::detect()->id(),
        ];

        file_put_contents(
            $manifestPath,
            json_encode($payload, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . PHP_EOL,
        );
    }

    private function isAlreadyInstalled(string $manifestPath, string $version): bool
    {
        if (!is_file($manifestPath)) {
            return false;
        }

        $contents = @file_get_contents($manifestPath);
        if ($contents === false) {
            return false;
        }

        try {
            $data = json_decode($contents, true, flags: JSON_THROW_ON_ERROR);
        } catch (\Throwable $e) {
            $this->io->writeError(sprintf('<comment>[rabbit-rs]</comment> Ignoring corrupt manifest: %s', $manifestPath));
            return false;
        }

        return is_array($data) && isset($data['version']) && $data['version'] === $version;
    }

    private function locatePluginPackage(): ?PackageInterface
    {
        $localRepo = $this->composer->getRepositoryManager()->getLocalRepository();
        $packages = $localRepo->findPackages(self::PACKAGE_NAME);

        return $packages[0] ?? null;
    }
}
