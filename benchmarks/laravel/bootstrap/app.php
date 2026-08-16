<?php

declare(strict_types=1);

require __DIR__ . '/../vendor/autoload.php';

use Illuminate\Container\Container;
use Illuminate\Events\Dispatcher;
use Illuminate\Foundation\Application;

Container::setInstance(new Application(
    dirname(__DIR__),
    new Dispatcher(),
));
