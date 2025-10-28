<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Installer;

use Composer\Composer;
use Composer\EventDispatcher\EventSubscriberInterface;
use Composer\IO\IOInterface;
use Composer\Plugin\PluginInterface;
use Composer\Script\Event;
use Composer\Script\ScriptEvents;

final class Plugin implements PluginInterface, EventSubscriberInterface
{
    private ?BinaryInstaller $installer = null;

    public function activate(Composer $composer, IOInterface $io): void
    {
        $this->installer = new BinaryInstaller($composer, $io);
    }

    public function deactivate(Composer $composer, IOInterface $io): void
    {
        // Nothing to clean up.
    }

    public function uninstall(Composer $composer, IOInterface $io): void
    {
        // Nothing to clean up.
    }

    public static function getSubscribedEvents(): array
    {
        return [
            ScriptEvents::POST_INSTALL_CMD => 'installBinary',
            ScriptEvents::POST_UPDATE_CMD => 'installBinary',
            ScriptEvents::POST_AUTOLOAD_DUMP => 'publishStub',
        ];
    }

    public function installBinary(Event $event): void
    {
        $this->ensureInstaller($event)->install();
    }

    public function publishStub(Event $event): void
    {
        $this->ensureInstaller($event)->publishStub();
    }

    private function ensureInstaller(Event $event): BinaryInstaller
    {
        if ($this->installer === null) {
            $this->installer = new BinaryInstaller($event->getComposer(), $event->getIO());
        }

        return $this->installer;
    }
}

