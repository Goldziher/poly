<?php

function greet($name)
{
    return "hello $name";
}

class Greeter
{
    public $name;

    public function __construct($name)
    {
        $this->name = $name;
    }
}
