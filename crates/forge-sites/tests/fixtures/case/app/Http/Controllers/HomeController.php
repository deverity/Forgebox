<?php

namespace App\Http\Controllers;

/**
 * Home page of the shop.
 *
 * Works on Windows (insensitive folder), breaks on Linux: the file is views/Header.php.
 */
class HomeController
{
    public function index()
    {
        require_once base_path('Views/header.php');

        return view('welcome');
    }
}
