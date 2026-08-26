package com.localsync.sample;

import java.util.List;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestBody;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
@RequestMapping("/api/notes")
public class NoteController {

    private final NoteRepository notes;

    public NoteController(NoteRepository notes) {
        this.notes = notes;
    }

    @GetMapping
    public List<Note> list() {
        return notes.findAll();
    }

    @PostMapping
    public Note create(@RequestBody Note note) {
        note.setId(null); // always insert, never let a client-supplied id trigger an update
        return notes.save(note);
    }
}
