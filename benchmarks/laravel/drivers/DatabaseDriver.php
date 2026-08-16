<?php

declare(strict_types=1);

namespace Drivers;

use Drivers\Support\MeasuresResources;
use PDO;

final class DatabaseDriver implements BenchmarkDriver
{
    use MeasuresResources;

    private ?PDO $pdo = null;
    private array $config;
    private int $consumed = 0;
    private array $seenIds = [];
    private int $duplicates = 0;
    private int $losses = 0;
    private string $table = 'bench_jobs';

    public function __construct(array $config = [])
    {
        $this->config = $config;
    }

    public function setup(): void
    {
        $connection = $this->config['connection'] ?? 'sqlite';
        $database = $this->config['database'] ?? ':memory:';

        try {
            if ($connection === 'sqlite') {
                $this->pdo = new PDO('sqlite:' . $database);
            } elseif ($connection === 'mysql') {
                $host = $this->config['host'] ?? '127.0.0.1';
                $this->pdo = new PDO(
                    "mysql:host={$host};dbname={$database}",
                    $this->config['user'] ?? 'root',
                    $this->config['password'] ?? '',
                );
            } else {
                $this->pdo = new PDO($connection . ':' . $database);
            }
            $this->pdo->setAttribute(PDO::ATTR_ERRMODE, PDO::ERRMODE_EXCEPTION);

            $this->pdo->exec("CREATE TABLE IF NOT EXISTS {$this->table} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                payload TEXT NOT NULL,
                available_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            )");
        } catch (\Throwable) {
            $this->pdo = null;
        }
    }

    public function publish(array $messages): void
    {
        if ($this->pdo === null) {
            return;
        }
        $now = time();
        $stmt = $this->pdo->prepare("INSERT INTO {$this->table} (payload, available_at, created_at) VALUES (?, ?, ?)");
        foreach ($messages as $message) {
            try {
                $stmt->execute([$message, $now, $now]);
            } catch (\Throwable) {
            }
        }
    }

    public function consume(int $count): void
    {
        if ($this->pdo === null) {
            return;
        }
        $this->consumed = 0;
        $this->seenIds = [];
        $this->duplicates = 0;
        $now = time();
        $this->startTimer();

        while ($this->consumed < $count) {
            try {
                $stmt = $this->pdo->prepare("SELECT id, payload FROM {$this->table} WHERE available_at <= ? ORDER BY id LIMIT 1");
                $stmt->execute([$now]);
                $row = $stmt->fetch(PDO::FETCH_ASSOC);
            } catch (\Throwable) {
                break;
            }
            if ($row === false) {
                break;
            }
            $payload = json_decode((string) $row['payload'], true);
            $msgId = $payload['id'] ?? null;
            if ($msgId !== null) {
                if (isset($this->seenIds[$msgId])) {
                    $this->duplicates++;
                }
                $this->seenIds[$msgId] = true;
            }
            try {
                $this->pdo->prepare("DELETE FROM {$this->table} WHERE id = ?")->execute([$row['id']]);
            } catch (\Throwable) {
            }
            $this->consumed++;
        }

        $this->losses = max(0, $count - $this->consumed);
    }

    public function reset(): void
    {
        if ($this->pdo !== null) {
            try {
                $this->pdo->exec("DELETE FROM {$this->table}");
            } catch (\Throwable) {
            }
        }
        $this->consumed = 0;
        $this->duplicates = 0;
        $this->losses = 0;
        $this->seenIds = [];
        $this->resetLatencies();
    }

    public function metrics(): array
    {
        return $this->buildMetrics(
            messageCount: $this->consumed,
            elapsedSeconds: $this->elapsedSeconds(),
            connections: $this->pdo !== null ? 1 : 0,
            channels: 0,
            duplicates: $this->duplicates,
            losses: $this->losses,
        );
    }
}
